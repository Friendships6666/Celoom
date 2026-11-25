"""Interactive GTK3 handoff viewer for the private desktop so users can complete login/2FA."""
import json
import os
import sys
import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Gdk", "3.0")
gi.require_version("GdkPixbuf", "2.0")
from gi.repository import Gdk, GdkPixbuf, GLib, Gtk


def main():
    frame_path, input_path, width, height = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])

    win = Gtk.Window(title="Celoom 私有桌面接管 · 请完成登录或验证后关闭此窗口")
    win.set_default_size(min(width, 1280), min(height, 800))

    event_box = Gtk.EventBox()
    image = Gtk.Image()
    event_box.add(image)
    event_box.set_can_focus(True)
    event_box.add_events(
        Gdk.EventMask.BUTTON_PRESS_MASK
        | Gdk.EventMask.SCROLL_MASK
        | Gdk.EventMask.KEY_PRESS_MASK
    )
    win.add(event_box)

    def emit(payload):
        try:
            with open(input_path, "a", encoding="utf-8") as f:
                f.write(json.dumps(payload, ensure_ascii=False) + "\n")
        except OSError:
            pass

    def scale_coords(ex, ey):
        alloc = image.get_allocation()
        if alloc.width <= 0 or alloc.height <= 0:
            return int(ex), int(ey)
        sx = int(round(ex * width / alloc.width))
        sy = int(round(ey * height / alloc.height))
        return max(0, min(width - 1, sx)), max(0, min(height - 1, sy))

    def on_button(_w, ev):
        event_box.grab_focus()
        x, y = scale_coords(ev.x, ev.y)
        btn = "right" if ev.button == 3 else "middle" if ev.button == 2 else "left"
        emit({"action": "click", "x": x, "y": y, "button": btn})
        return True

    def on_scroll(_w, ev):
        x, y = scale_coords(ev.x, ev.y)
        delta = 120 if ev.direction == Gdk.ScrollDirection.DOWN else -120 if ev.direction == Gdk.ScrollDirection.UP else 0
        if delta:
            emit({"action": "scroll", "x": x, "y": y, "delta": delta})
        return True

    def on_key(_w, ev):
        u = Gdk.keyval_to_unicode(ev.keyval)
        name = Gdk.keyval_name(ev.keyval) or ""
        special = {
            "Return": "Return",
            "BackSpace": "BackSpace",
            "Tab": "Tab",
            "Escape": "Escape",
            "Left": "Left",
            "Right": "Right",
            "Up": "Up",
            "Down": "Down",
        }
        if name in special:
            emit({"action": "key", "key": special[name]})
            return True
        if u and u >= 32:
            emit({"action": "type", "text": chr(u)})
            return True
        return False

    event_box.connect("button-press-event", on_button)
    event_box.connect("scroll-event", on_scroll)
    win.connect("key-press-event", on_key)
    win.connect("destroy", Gtk.main_quit)

    last_mtime = [0.0]

    def refresh():
        try:
            st = os.stat(frame_path)
            if st.st_mtime != last_mtime[0] and st.st_size > 0:
                pb = GdkPixbuf.Pixbuf.new_from_file(frame_path)
                image.set_from_pixbuf(pb)
                last_mtime[0] = st.st_mtime
        except Exception:
            pass
        return True

    GLib.timeout_add(160, refresh)
    win.show_all()
    event_box.grab_focus()
    Gtk.main()


if __name__ == "__main__":
    main()
