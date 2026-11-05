"""Test original tool declarations without dynamic registration, using synthetic auth.

Records only protocol facts. Never connects to a real model, enables shell/image
execution, or loads the operator's credentials. No relay policy is changed.
"""
import http.server
import json
from pathlib import Path
import sys
import tempfile
import threading
import time

from probe_native_output_limit import INSTALL, Native, ROOT

SEEN = []
ACTIVE = {}


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        assert self.path == '/v1/responses'
        assert self.headers['Authorization'] == 'Bearer SYNTHETIC_LIMIT_PROBE'
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        SEEN.append(body)
        outputs = [i for i in body['input'] if i.get('type') in ('function_call_output', 'custom_tool_call_output') and i.get('call_id') == ACTIVE['call']['call_id']]
        item = ({'type':'message','id':'finished','role':'assistant','content':[{'type':'output_text','text':'Synthetic response'}]}
                if outputs else ACTIVE['call'])
        events = [{'type':'response.output_item.done','output_index':0,'item':item},
                  {'type':'response.completed','response':{'id':f'probe_{len(SEEN)}','status':'completed','output':[item],
                   'usage':{'input_tokens':10,'output_tokens':2,'total_tokens':12}}}]
        data = ''.join(f'data: {json.dumps(e)}\n\n' for e in events).encode()
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
            pass


def main():
    model = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=model.serve_forever, daemon=True).start()
    report = {'nativeVersion':INSTALL['appServerVersion'], 'realModelRequests':0, 'cases':[]}
    with tempfile.TemporaryDirectory(prefix='original-tools-probe-', dir=ROOT/'.cache') as temporary:
        root = Path(temporary).resolve()
        assert root.is_relative_to((ROOT/'.cache').resolve())
        native = Native(root, model.server_port, enabled_features=('hooks',))
        try:
            assert 'result' in native.call('initialize', {'clientInfo':INSTALL['nativeClientInfo'], 'capabilities':{'experimentalApi':True}})
            native.send({'method':'initialized'})
            sample = native.call('thread/sample', {'threadId':'00000000-0000-0000-0000-000000000000',
                'expectedTurnId':'source-turn','prompt':'Synthetic capability check'})
            report['sampleRpcAvailability'] = sample.get('error', {'unexpected':'accepted'})
            params = {'model':'gpt-6.1-sol','modelProvider':'synthetic','cwd':str(native.cwd),'ephemeral':True,
                      'approvalPolicy':'untrusted','sandbox':'read-only','baseInstructions':'','developerInstructions':'',
                      'dynamicTools':[],'environments':[],'runtimeWorkspaceRoots':[],'selectedCapabilityRoots':[],
                      'experimentalRawEvents':True,'config':{'project_doc_max_bytes':0,
                      'features.code_mode':{'enabled':False,'direct_only_tool_namespaces':['functions','client'],
                                            'excluded_tool_namespaces':['clock','collaboration']}}}
            reserved = native.call('thread/start', {**params,'dynamicTools':[{'type':'namespace','name':'functions',
                'description':'Caller tools','tools':[{'type':'function','name':'exec','description':'Caller code',
                'inputSchema':{'type':'object'}}]}]})
            report['reservedDynamicRegistration'] = reserved.get('error', {'unexpected':'accepted'})
            cases = [
                ('client_function', {'type':'namespace','name':'client','description':'Client tools','tools':[{'type':'function','name':'echo','parameters':{'type':'object'}}]},
                 {'type':'function_call','id':'call_item','call_id':'caller_only','namespace':'client','name':'echo','arguments':'{}'}),
                ('desktop_custom', {'type':'namespace','name':'functions','description':'Client tools','tools':[{'type':'custom','name':'exec','format':{'type':'text'}}]},
                 {'type':'custom_tool_call','id':'call_item','call_id':'caller_only','namespace':'functions','name':'exec','input':"throw new Error('PROBE_MUST_STAY_ON_CLIENT')"}),
            ]
            for label, declaration, call in cases:
                ACTIVE['call'] = call
                before = len(SEEN)
                started = native.call('thread/start', params)
                assert 'result' in started, started
                thread = started['result']['thread']['id']
                injected = native.call('thread/inject_items', {'threadId':thread,'items':[
                    {'type':'additional_tools','role':'developer','tools':[declaration]},
                    {'type':'message','role':'user','content':[{'type':'input_text','text':label}]}]})
                assert 'result' in injected, injected
                assert 'result' in native.call('turn/start', {'threadId':thread,'input':[]})
                completed = None
                methods = []
                deadline = time.monotonic()+30
                while time.monotonic() < deadline:
                    event = native.receive(max(0.1,deadline-time.monotonic()))
                    methods.append(event.get('method'))
                    if event.get('method') == 'turn/completed':
                        completed = event['params']['turn']['status']
                        break
                captured = SEEN[before:]
                assert completed is not None and captured
                declarations = [t for i in captured[0]['input'] if i.get('type') == 'additional_tools' for t in i['tools']]
                native_outputs = [i for b in captured[1:] for i in b['input'] if i.get('type') in ('function_call_output','custom_tool_call_output') and i.get('call_id') == 'caller_only']
                report['cases'].append({'case':label,'originalDeclarationPreserved':declaration in declarations,
                    'lastSameNamespaceDeclarationMatches':next((t for t in reversed(declarations) if t.get('name') == declaration.get('name')),None) == declaration,
                    'nativeStatus':completed,'modelRequests':len(captured),'nativeGeneratedToolOutputs':native_outputs,
                    'requestedCallerExecution':'item/tool/call' in methods,
                    'hasRawResponseBoundary':'rawResponse/completed' in methods})
            # Sync post-model hooks defer dispatch in the official Core loop.
            # Probe this separately; observation hooks are not assumed to be a
            # reliable production execution-control API without fault tests.
            hook = root/'post_model.py'
            marker = root/'post-model-started'
            release = root/'release-post-model'
            hook.write_text('import sys,json,time\nfrom pathlib import Path\njson.load(sys.stdin)\n'
                + f'Path({str(marker)!r}).write_text("started")\n'
                + f'while not Path({str(release)!r}).exists(): time.sleep(0.02)\n', encoding='utf-8')
            command = f'"{sys.executable}" "{hook}"'
            label, declaration, call = cases[1]
            ACTIVE['call'] = call
            before = len(SEEN)
            handlers = [{'hooks':[{'type':'command','command':command,'commandWindows':command,'timeout':10,'async':False}]}]
            written = native.call('config/batchWrite', {'edits':[{'keyPath':'hooks.PostModelRequest','value':handlers,'mergeStrategy':'upsert'}], 'reloadUserConfig':True})
            assert 'result' in written, written
            listed = native.call('hooks/list', {'cwds':[str(native.cwd)]})
            hooks = [h for group in listed['result']['data'] for h in group['hooks'] if h['eventName'] == 'postModelRequest']
            assert hooks, listed
            trust = {h['key']:{'trusted_hash':h['currentHash']} for h in hooks}
            assert 'result' in native.call('config/batchWrite', {'edits':[{'keyPath':'hooks.state','value':trust,'mergeStrategy':'upsert'}], 'reloadUserConfig':True})
            config = params['config']
            started = native.call('thread/start', {**params,'config':config})
            assert 'result' in started, started
            thread = started['result']['thread']['id']
            assert 'result' in native.call('thread/inject_items', {'threadId':thread,'items':[
                {'type':'additional_tools','role':'developer','tools':[declaration]},
                {'type':'message','role':'user','content':[{'type':'input_text','text':'custom with sync observer'}]}]})
            turn = native.call('turn/start', {'threadId':thread,'input':[]})
            assert 'result' in turn, turn
            deadline = time.monotonic()+20
            while time.monotonic()<deadline:
                event = native.receive(max(0.1,deadline-time.monotonic()))
                if event.get('method') == 'rawResponse/completed': break
            deadline = time.monotonic()+2
            while not marker.exists() and time.monotonic()<deadline: time.sleep(0.02)
            pre_interrupt = len(SEEN)-before
            interrupted = native.call('turn/interrupt', {'threadId':thread,'turnId':turn['result']['turn']['id']})
            release.write_text('release',encoding='utf-8')
            time.sleep(0.2)
            report['syncPostModelHook'] = {'hookStarted':marker.exists(), 'requestsBeforeInterrupt':pre_interrupt,
                'requestsAfterInterrupt':len(SEEN)-before, 'interruptAcknowledged':'result' in interrupted}
            # A hook startup failure must not be mistaken for an execution gate.
            failed_hook = root/'failed_post_model.py'
            failed_hook.write_text('raise SystemExit(1)\n',encoding='utf-8')
            failed_command = f'"{sys.executable}" "{failed_hook}"'
            handlers = [{'hooks':[{'type':'command','command':failed_command,'commandWindows':failed_command,'timeout':10,'async':False}]}]
            assert 'result' in native.call('config/batchWrite', {'edits':[{'keyPath':'hooks.PostModelRequest','value':handlers,'mergeStrategy':'replace'}], 'reloadUserConfig':True})
            listed = native.call('hooks/list', {'cwds':[str(native.cwd)]})
            hooks = [h for group in listed['result']['data'] for h in group['hooks'] if h['eventName']=='postModelRequest']
            assert 'result' in native.call('config/batchWrite', {'edits':[{'keyPath':'hooks.state','value':{h['key']:{'trusted_hash':h['currentHash']} for h in hooks},'mergeStrategy':'upsert'}], 'reloadUserConfig':True})
            before = len(SEEN)
            started = native.call('thread/start',params)
            assert 'result' in started,started
            thread = started['result']['thread']['id']
            assert 'result' in native.call('thread/inject_items', {'threadId':thread,'items':[
                {'type':'additional_tools','role':'developer','tools':[declaration]},
                {'type':'message','role':'user','content':[{'type':'input_text','text':'failed observer'}]}]})
            assert 'result' in native.call('turn/start', {'threadId':thread,'input':[]})
            deadline=time.monotonic()+20
            while time.monotonic()<deadline:
                event=native.receive(max(0.1,deadline-time.monotonic()))
                if event.get('method')=='turn/completed': break
            report['failedSyncPostModelHook'] = {'modelRequests':len(SEEN)-before,
                'nativeGeneratedToolOutputs': [i for b in SEEN[before+1:] for i in b['input'] if i.get('type')=='custom_tool_call_output' and i.get('call_id')=='caller_only']}
        finally:
            native.close()
            model.shutdown()
            model.server_close()
    output = ROOT/'.cache/native-original-tools-probe.json'
    output.write_text(json.dumps(report,ensure_ascii=False,indent=2),encoding='utf-8')
    print(json.dumps(report,ensure_ascii=False,indent=2))


if __name__ == '__main__':
    main()
