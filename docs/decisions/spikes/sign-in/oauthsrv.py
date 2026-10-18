# Minimal MCP-over-HTTP server that demands OAuth, with an authorization server (DCR + PKCE) on the same port.
# usage: oauthsrv.py <port> [<cert.pem> <key.pem> <host>]  (with a certificate it serves https://<host>:<port>)
import http.server, json, sys, urllib.parse, secrets, time, ssl
PORT=int(sys.argv[1])
TLS=len(sys.argv)>=5
BASE=f"https://{sys.argv[4]}:{PORT}" if TLS else f"http://127.0.0.1:{PORT}"
LOG=open("srv.log","a")
codes={}; tokens=set()
def log(*a):
    print(time.strftime("%H:%M:%S"),*a,file=LOG,flush=True)
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self,*a): pass
    def send(self,code,body,ctype="application/json",hdrs={}):
        b=body if isinstance(body,bytes) else json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type",ctype)
        for k,v in hdrs.items(): self.send_header(k,v)
        self.send_header("Content-Length",str(len(b)))
        self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        log("GET",self.path)
        u=urllib.parse.urlparse(self.path)
        if u.path.startswith("/.well-known/oauth-protected-resource"):
            return self.send(200,{"resource":BASE+"/mcp","authorization_servers":[BASE]})
        if u.path.startswith("/.well-known/oauth-authorization-server") or u.path.startswith("/.well-known/openid-configuration"):
            return self.send(200,{"issuer":BASE,"authorization_endpoint":BASE+"/authorize","token_endpoint":BASE+"/token","registration_endpoint":BASE+"/register","response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"],"token_endpoint_auth_methods_supported":["none","client_secret_post","client_secret_basic"]})
        if u.path=="/authorize":
            q=dict(urllib.parse.parse_qsl(u.query))
            log("AUTHORIZE",json.dumps(q))
            code=secrets.token_hex(8); codes[code]=q
            loc=q["redirect_uri"]+("&" if "?" in q["redirect_uri"] else "?")+urllib.parse.urlencode({"code":code,"state":q.get("state","")})
            log("REDIRECT",loc)
            return self.send(302,b"",hdrs={"Location":loc})
        return self.send(401,{"error":"unauthorized"},hdrs={"WWW-Authenticate":f'Bearer resource_metadata="{BASE}/.well-known/oauth-protected-resource"'})
    def do_POST(self):
        n=int(self.headers.get("Content-Length",0)); body=self.rfile.read(n).decode()
        log("POST",self.path,body[:500], "auth=",self.headers.get("Authorization"))
        if self.path=="/register":
            req=json.loads(body)
            return self.send(201,{"client_id":"cid-"+secrets.token_hex(4),"redirect_uris":req.get("redirect_uris"),"token_endpoint_auth_method":"none"})
        if self.path=="/token":
            tok=secrets.token_hex(8); tokens.add(tok)
            return self.send(200,{"access_token":tok,"token_type":"Bearer","expires_in":3600,"refresh_token":"r"+tok})
        if self.path.startswith("/mcp"):
            a=self.headers.get("Authorization","")
            if a.startswith("Bearer ") and a[7:] in tokens:
                req=json.loads(body)
                if req.get("method")=="initialize":
                    return self.send(200,{"jsonrpc":"2.0","id":req["id"],"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"spike","version":"1"}}})
                if "id" in req:
                    return self.send(200,{"jsonrpc":"2.0","id":req["id"],"result":{"tools":[]}})
                return self.send(202,b"")
        return self.send(401,{"error":"unauthorized"},hdrs={"WWW-Authenticate":f'Bearer resource_metadata="{BASE}/.well-known/oauth-protected-resource"'})
srv=http.server.ThreadingHTTPServer(("127.0.0.1",PORT),H)
if TLS:
    ctx=ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER); ctx.load_cert_chain(sys.argv[2],sys.argv[3])
    srv.socket=ctx.wrap_socket(srv.socket,server_side=True)
srv.serve_forever()
