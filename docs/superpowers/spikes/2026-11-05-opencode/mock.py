import json,sys,time
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
LOG=sys.argv[2]
def text_of(m):
    c=m.get('content')
    if isinstance(c,str): return c
    return ' '.join(x.get('text','') for x in (c or []) if isinstance(x,dict))
class H(BaseHTTPRequestHandler):
    protocol_version='HTTP/1.1'
    def log_message(self,*a): pass
    def do_GET(self):
        b=b'{"object":"list","data":[{"id":"m","object":"model"}]}'
        self.send_response(200); self.send_header('content-type','application/json'); self.send_header('content-length',str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_POST(self):
        body=json.loads(self.rfile.read(int(self.headers.get('content-length',0))))
        msgs=body.get('messages',[])
        last=msgs[-1] if msgs else {}
        with open(LOG,'a') as f: f.write(json.dumps({'path':self.path,'roles':[m.get('role') for m in msgs],'last':text_of(last)[:300],'lastrole':last.get('role'),'tools':[t['function']['name'] for t in body.get('tools',[])][:30]})+'\n')
        self.send_response(200); self.send_header('content-type','text/event-stream'); self.send_header('cache-control','no-cache'); self.send_header('connection','close'); self.end_headers()
        def ev(delta,finish=None,usage=None):
            o={'id':'c1','object':'chat.completion.chunk','created':int(time.time()),'model':'m','choices':[{'index':0,'delta':delta,'finish_reason':finish}]}
            if usage: o['usage']=usage
            self.wfile.write(('data: '+json.dumps(o)+'\n\n').encode()); self.wfile.flush()
        t=text_of(last); usage={'prompt_tokens':600,'completion_tokens':20,'total_tokens':620}
        if last.get('role')=='user' and 'TOOL:' in t:
            cmd=t.split('TOOL:',1)[1].strip()
            ev({'role':'assistant','tool_calls':[{'index':0,'id':'call_1','type':'function','function':{'name':'shell','arguments':json.dumps({'command':cmd,'description':'spike'})}}]})
            ev({},'tool_calls',usage)
        else:
            if 'SLOW' in t: time.sleep(6)
            ev({'role':'assistant','content':'mock says: '+t[:60].replace('\n',' ')})
            ev({},'stop',usage)
        self.wfile.write(b'data: [DONE]\n\n'); self.wfile.flush(); self.close_connection=True
ThreadingHTTPServer(('127.0.0.1',int(sys.argv[1])),H).serve_forever()
