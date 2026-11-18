"""Fixed, embedded JustAI private-desktop bootstrap and AT-SPI bridge.

This source is compiled into the Rust binary. It never evaluates user code.
All modes require the private runtime and D-Bus environment established by Rust.
"""
import json
import os
import sys
import time


def private_environment():
    runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    address = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "")
    if not runtime or not os.path.basename(runtime).startswith("justai-desktop-"):
        raise RuntimeError("private desktop runtime required")
    st = os.stat(runtime)
    if st.st_uid != os.getuid() or st.st_mode & 0o077:
        raise RuntimeError("private desktop runtime permissions invalid")
    if not address.startswith("unix:"):
        raise RuntimeError("private session bus required")


def bootstrap():
    private_environment()
    ready, display, kwin = sys.argv[2:5]
    # dbus-run-session created this address after Rust cleared the host environment.
    with open(ready, "x", encoding="utf-8") as stream:
        json.dump({"bus_address": os.environ["DBUS_SESSION_BUS_ADDRESS"],
                   "display": display}, stream)
    os.execv(kwin, [kwin, "--virtual", "--socket", display, "--width", "1280",
                   "--height", "800", "--no-lockscreen", "--no-global-shortcuts",
                   "--no-kactivities"])


def bridge():
    private_environment()
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi, GLib
    Atspi.init()
    Atspi.set_timeout(900, 900)
    generation = 0
    refs = {}
    allowed = set()

    def managed(pid):
        # Include descendants of explicitly launched apps; never traverse host apps.
        seen = set()
        for _ in range(32):
            if pid in allowed:
                return True
            if pid <= 1 or pid in seen:
                break
            seen.add(pid)
            try:
                with open("/proc/%d/status" % pid, encoding="utf-8") as stream:
                    parent = next(line for line in stream if line.startswith("PPid:"))
                pid = int(parent.split()[1])
            except (OSError, ValueError, StopIteration):
                break
        return False

    def state(acc):
        return acc.get_state_set()

    def valid(acc):
        return not state(acc).contains(Atspi.StateType.DEFUNCT)

    def get_ref(ref):
        entry = refs.get(ref)
        if entry is None:
            raise RuntimeError("stale accessibility reference; observe again")
        acc, app, window, pid = entry
        if not managed(pid) or not valid(acc) or not valid(window):
            raise RuntimeError("accessibility target is no longer available")
        if acc.get_application().get_process_id() != pid:
            raise RuntimeError("accessibility target changed")
        # The original top-level window must still belong to the same app.
        if not any(app.get_child_at_index(i) == window
                   for i in range(min(app.get_child_count(), 128))):
            raise RuntimeError("accessibility window closed; observe again")
        return acc

    def observe():
        nonlocal generation, refs
        generation += 1
        refs = {}
        nodes = []
        started = time.monotonic()
        desktop = Atspi.get_desktop(0)

        def walk(acc, app, window, pid, parent, depth):
            if len(nodes) >= 512 or depth > 12 or time.monotonic() - started > 1.2:
                return
            if not valid(acc):
                return
            states = state(acc)
            # Invisible/hidden content (including values) is never exported.
            if depth > 0 and not states.contains(Atspi.StateType.SHOWING):
                return
            ref = "ax-%d-%d" % (generation, len(nodes) + 1)
            refs[ref] = (acc, app, window, pid)
            role = acc.get_role()
            node = {"ref_id": ref, "parent": parent, "pid": pid,
                    "role": acc.get_role_name(), "name": (acc.get_name() or "")[:500]}
            if role == Atspi.Role.PASSWORD_TEXT:
                node["protected"] = True
            else:
                try:
                    text = acc.get_text_iface()
                    if text is not None:
                        # GI returns an Accessible implementing Text: call the
                        # interface explicitly (Accessible.get_text is a different,
                        # deprecated zero-argument method on some distros).
                        node["text"] = Atspi.Text.get_text(text, 0, min(Atspi.Text.get_character_count(text), 1000))
                except Exception:
                    pass
            try:
                component = acc.get_component_iface()
                if component is not None:
                    rect = component.get_extents(Atspi.CoordType.SCREEN)
                    node["rect"] = [rect.x, rect.y, rect.width, rect.height]
            except Exception:
                pass
            nodes.append(node)
            for i in range(min(acc.get_child_count(), 128)):
                try:
                    child = acc.get_child_at_index(i)
                    if child is not None:
                        walk(child, app, window, pid, ref, depth + 1)
                except Exception:
                    continue

        if desktop is not None:
            for i in range(min(desktop.get_child_count(), 128)):
                app = desktop.get_child_at_index(i)
                pid = app.get_process_id()
                if not managed(pid):
                    continue
                for j in range(min(app.get_child_count(), 64)):
                    window = app.get_child_at_index(j)
                    if window is not None:
                        walk(window, app, window, pid, None, 0)
        return {"generation": generation, "nodes": nodes,
                "truncated": len(nodes) >= 512 or time.monotonic() - started > 1.2,
                "scope": "managed apps on private accessibility bus"}

    for line in sys.stdin:
        try:
            while GLib.MainContext.default().pending():
                GLib.MainContext.default().iteration(False)
            if len(line) > 131072:
                raise RuntimeError("accessibility request too large")
            request = json.loads(line)
            allowed = {int(pid) for pid in request.get("pids", []) if int(pid) > 1}
            action = request["action"]
            if action == "observe":
                result = observe()
            elif action == "click":
                acc = get_ref(request.get("ref_id"))
                iface = acc.get_action_iface()
                if iface is None:
                    raise RuntimeError("control has no accessibility action; use private coordinates")
                count = iface.get_n_actions()
                preferred = next((i for i in range(count) if iface.get_action_name(i).lower()
                                  in ("click", "press", "activate", "toggle")), None)
                if preferred is None:
                    if count != 1:
                        raise RuntimeError("control has no unambiguous click action")
                    preferred = 0
                if not iface.do_action(preferred):
                    raise RuntimeError("accessibility click was rejected")
                result = {"clicked": request["ref_id"]}
            elif action in ("fill", "type"):
                acc = get_ref(request.get("ref_id"))
                editable = acc.get_editable_text_iface()
                if editable is None:
                    raise RuntimeError("control does not accept accessible text editing")
                text = request.get("text", "")
                if action == "fill":
                    accepted = editable.set_text_contents(text)
                else:
                    text_iface = acc.get_text_iface()
                    if text_iface is None:
                        raise RuntimeError("control does not expose an insertion caret")
                    accepted = editable.insert_text(text_iface.get_caret_offset(), text, len(text.encode("utf-8")))
                if not accepted:
                    raise RuntimeError("accessible text edit was rejected")
                result = {"edited": request["ref_id"], "operation": action}
            else:
                raise RuntimeError("unsupported accessibility operation")
            print(json.dumps({"ok": True, "result": result}, ensure_ascii=True), flush=True)
        except Exception as exc:
            print(json.dumps({"ok": False, "error": str(exc)[:500]}), flush=True)


def fixture():
    """Only invoked by the ignored Rust smoke test in its newly created desktop."""
    private_environment()
    import gi
    gi.require_version("Gtk", "3.0")
    from gi.repository import Gtk
    window = Gtk.Window(title="JustAI Private Desktop Fixture")
    window.set_default_size(640, 400)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16)
    entry = Gtk.Entry()
    entry.get_accessible().set_name("Fixture input")
    button = Gtk.Button(label="Fixture click")
    label = Gtk.Label(label="Fixture ready")
    button.connect("clicked", lambda _: label.set_text("Fixture clicked"))
    for widget in (entry, button, label):
        box.pack_start(widget, False, False, 12)
    window.add(box)
    window.connect("destroy", Gtk.main_quit)
    window.show_all()
    Gtk.main()


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit("fixed helper mode required")
    mode = sys.argv[1]
    if mode == "bootstrap":
        bootstrap()
    elif mode == "bridge":
        bridge()
    elif mode == "fixture":
        fixture()
    else:
        raise SystemExit("unknown fixed helper mode")
