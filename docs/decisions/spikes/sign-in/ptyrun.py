# usage: ptyrun.py <seconds> [--paste-after-url] [--curl-after-url] -- cmd...
import os, pty, sys, time, select, re, subprocess, urllib.request
secs=float(sys.argv[1]); mode=sys.argv[2]; cmd=sys.argv[sys.argv.index("--")+1:]
pid,fd=pty.fork()
if pid==0:
    os.execvp(cmd[0],cmd)
buf=b""; t0=time.time(); acted=False
while time.time()-t0<secs:
    r,_,_=select.select([fd],[],[],0.2)
    if r:
        try: d=os.read(fd,4096)
        except OSError: break
        if not d: break
        buf+=d; sys.stdout.write(f"[{time.time()-t0:5.2f}] {d!r}\n"); sys.stdout.flush()
    m=re.search(rb"https?://127\.0\.0\.1:8765/authorize[^\x00-\x20\x7f]+",buf)
    if m and not acted:
        acted=True; url=m.group(0).decode()
        # follow the authorize redirect manually to get the callback URL
        req=urllib.request.Request(url)
        class NR(urllib.request.HTTPRedirectHandler):
            def redirect_request(self,*a,**k): return None
        op=urllib.request.build_opener(NR)
        try: op.open(req)
        except urllib.error.HTTPError as e: loc=e.headers["Location"]
        print("CALLBACK URL:",loc,flush=True)
        if mode=="paste":
            time.sleep(0.5); os.write(fd,(loc+"\r").encode())
        elif mode=="curl":
            time.sleep(0.5)
            try: print("CURL RESULT:", urllib.request.urlopen(loc,timeout=5).read()[:300],flush=True)
            except Exception as e: print("CURL ERR",e,flush=True)
        elif mode=="badpaste":
            time.sleep(0.5); os.write(fd,(loc.replace("state=","state=x")+"\r").encode())
        elif mode=="garbage":
            time.sleep(0.5); os.write(fd,b"hello\r")
        elif mode=="ctrlc":
            time.sleep(0.5); os.write(fd,b"\x03")
        elif mode=="ss":
            print(subprocess.run(["ss","-ltnp"],capture_output=True,text=True).stdout,flush=True)
try:
    _,st=os.waitpid(pid,os.WNOHANG); print("WAIT",st)
except Exception as e: print(e)
