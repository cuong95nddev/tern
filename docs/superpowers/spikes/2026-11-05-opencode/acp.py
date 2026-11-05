import json,subprocess,sys,threading,time,os
# usage: acp.py SESSION_TO_LOAD PROMPT ; prints every line of the protocol, cut
oc=os.environ['OC']; load=sys.argv[1]; prompt=sys.argv[2]; cwd=os.getcwd()
p=subprocess.Popen([oc,'acp'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,bufsize=1)
ids=[0]; waits={}
def send(m,params):
    ids[0]+=1; i=ids[0]; ev=threading.Event(); waits[i]=[ev,None]
    p.stdin.write(json.dumps({'jsonrpc':'2.0','id':i,'method':m,'params':params})+'\n'); p.stdin.flush(); return i
def wait(i,t=45):
    ev=waits[i][0]; ev.wait(t); return waits[i][1]
kinds={}
def reader():
    for l in p.stdout:
        try:o=json.loads(l)
        except: print('RAW',l[:200]);continue
        if 'id' in o and 'method' not in o:
            if o['id'] in waits: waits[o['id']][1]=o; waits[o['id']][0].set()
        elif 'method' in o and 'id' in o:
            print('REQUEST',json.dumps(o)[:700])
            opts=o['params'].get('options',[])
            pick=next((x for x in opts if x.get('kind')=='allow_once'),opts[0] if opts else None)
            r={'outcome':{'outcome':'selected','optionId':pick['optionId']}} if pick else {}
            p.stdin.write(json.dumps({'jsonrpc':'2.0','id':o['id'],'result':r})+'\n'); p.stdin.flush()
        else:
            u=o.get('params',{}).get('update',{}); k=u.get('sessionUpdate') or o.get('method')
            kinds[k]=kinds.get(k,0)+1
            if kinds[k]<=3 and k not in ('available_commands_update','agent_message_chunk'): print('NOTE',k,json.dumps({k2:v for k2,v in u.items() if k2 not in ('locations','rawInput')})[:900])
threading.Thread(target=reader,daemon=True).start()
r=wait(send('initialize',{'protocolVersion':1,'clientCapabilities':{'fs':{'readTextFile':False,'writeTextFile':False}},'clientInfo':{'name':'tern-spike','version':'0'}}))

if load!='-':
    r=wait(send('session/load',{'sessionId':load,'cwd':cwd,'mcpServers':[]})); print('LOAD',list((r.get('result') or r).keys()),[c.get('id') for c in r.get('result',{}).get('configOptions',[])],json.dumps(r.get('result',{}).get('modes'))[:300]); sid=load
else:
    r=wait(send('session/new',{'cwd':cwd,'mcpServers':[]})); print('NEW',json.dumps(r)[:1100]); sid=r['result']['sessionId']
for pr in sys.argv[2:]:
    print('=== PROMPT',pr)
    i=send('session/prompt',{'sessionId':sid,'prompt':[{'type':'text','text':pr}]})
    if 'SLOW' in pr:
        time.sleep(1.5); p.stdin.write(json.dumps({'jsonrpc':'2.0','method':'session/cancel','params':{'sessionId':sid}})+'\n'); p.stdin.flush(); print('cancel sent')
    print('RESULT',json.dumps(wait(i))[:400])
print('KINDS',kinds); p.terminate(); time.sleep(1); p.kill()
