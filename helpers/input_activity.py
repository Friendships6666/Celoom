#!/usr/bin/env python3
"""Fixed privileged input-activity bridge. Emits timing only, NEVER keys/codes.
No grabs and no injection. Physical input wins over AI host-desktop actions.
"""
import glob,json,os,selectors,struct,sys,time
EVENT=struct.Struct('llHHI')
selector=selectors.DefaultSelector();handles={};last=time.monotonic();last_scan=0;complete=False

def scan():
    global handles,complete
    available=set()
    for path in glob.glob('/dev/input/event*'):
        name=os.path.basename(path)
        device=os.path.realpath('/sys/class/input/'+name+'/device')
        if '/virtual/' in device:continue
        available.add(path)
        if path in handles:continue
        try:
            fd=os.open(path,os.O_RDONLY|os.O_NONBLOCK|os.O_CLOEXEC)
            selector.register(fd,selectors.EVENT_READ,path);handles[path]=fd
        except OSError:pass
    complete=bool(available) and available.issubset(handles)
    for path in list(handles):
        if path not in available:
            fd=handles.pop(path)
            try:selector.unregister(fd);os.close(fd)
            except OSError:pass

try:
    while True:
        now=time.monotonic()
        if now-last_scan>2:scan();last_scan=now
        for key,_ in selector.select(.05):
            try:
                data=os.read(key.fd,EVENT.size*128)
                if any(EVENT.unpack_from(data,n)[2] in (1,2,3) for n in range(0,len(data)-EVENT.size+1,EVENT.size)):last=time.monotonic()
            except OSError:
                try:selector.unregister(key.fd);os.close(key.fd)
                except OSError:pass
                handles.pop(key.data,None)
        print(json.dumps({'ready':bool(handles) and complete,'devices':len(handles),'idle_ms':int((time.monotonic()-last)*1000)}),flush=True)
except (BrokenPipeError,KeyboardInterrupt):pass
finally:
    for fd in handles.values():
        try:os.close(fd)
        except OSError:pass
