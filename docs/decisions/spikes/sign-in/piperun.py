import subprocess, sys, re, time, urllib.request, threading, os
mode=sys.argv[1]; name=sys.argv[2]
p=subprocess.Popen(["claude","mcp","login",name,"--no-browser"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=dict(os.environ,NO_COLOR="1"))
out=b""; err=b""
def rd(s,tag):
    global out
    while True:
        d=os.read(s.fileno(),4096)
        if not d: break
        print(tag,repr(d)[:400],flush=True); out+=d
threading.Thread(target=rd,args=(p.stdout,"OUT"),daemon=True).start()
threading.Thread(target=rd,args=(p.stderr,"ERR"),daemon=True).start()
t0=time.time(); acted=False
while time.time()-t0<10 and p.poll() is None:
    m=re.search(rb"http://127\.0\.0\.1:8765/authorize[^\x00-\x20\x7f]+",out)
    if m and not acted:
        acted=True
        class NR(urllib.request.HTTPRedirectHandler):
            def redirect_request(self,*a,**k): return None
        try: urllib.request.build_opener(NR).open(m.group(0).decode())
        except urllib.error.HTTPError as e: loc=e.headers["Location"]
        print("CB",loc)
        if mode=="paste": p.stdin.write((loc+"\n").encode()); p.stdin.flush()
        elif mode=="curl": urllib.request.urlopen(loc,timeout=5).read()
        elif mode=="cancel": p.stdin.close()
    time.sleep(0.1)
print("EXIT",p.poll(), time.time()-t0)
p.kill()
