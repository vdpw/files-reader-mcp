#!/usr/bin/env python3
"""Actual loopback HTTP smoke test; no global MCP configuration changes."""
import argparse
import json
import pathlib
import re
import select
import signal
import subprocess
import tempfile
import urllib.request

parser = argparse.ArgumentParser()
parser.add_argument('--binary', default='target/debug/files-reader-mcp')
args = parser.parse_args()
binary = pathlib.Path(args.binary).resolve()
with tempfile.TemporaryDirectory(prefix='files-reader-http-') as tmp:
    root = pathlib.Path(tmp)
    (root / 'hello.txt').write_bytes(b'  hello\r\nworld\n')
    config = root / 'server.toml'
    config.write_text('listen = "127.0.0.1:0"\n[[roots]]\nname = "sample"\npath = ' + json.dumps(tmp) + '\n')
    process = subprocess.Popen([str(binary), '--config', str(config)], stderr=subprocess.PIPE, text=True)
    try:
        assert select.select([process.stderr], [], [], 15)[0], 'startup timed out'
        line = process.stderr.readline()
        match = re.search(r'http://127\.0\.0\.1:\d+/mcp', line)
        assert match, line
        url = match.group(0)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        def rpc(method, params):
            req = urllib.request.Request(url, json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': method, 'params': params}).encode(), headers={'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream', 'MCP-Protocol-Version': '2025-11-25'})
            with opener.open(req, timeout=10) as response:
                assert response.status == 200
                value = json.load(response)
                assert 'error' not in value, value
                return value['result']
        initialized = rpc('initialize', {'protocolVersion': '2025-11-25', 'capabilities': {}, 'clientInfo': {'name': 'loopback-smoke', 'version': '1'}})
        assert initialized['serverInfo']['name'] == 'files-reader-mcp'
        listing = rpc('tools/list', {})
        assert len(listing['tools']) == 18
        read = rpc('tools/call', {'name': 'read_file', 'arguments': {'root': 'sample', 'path': 'hello.txt'}})
        assert not read.get('isError'), read
        data = json.loads(read['content'][0]['text'])
        assert ''.join(row['text'] for row in data['lines']) == '  hello\r\nworld\n'
        denied = rpc('tools/call', {'name': 'read_file', 'arguments': {'root': 'sample', 'path': '../outside'}})
        assert denied.get('isError') is True
        print(json.dumps({'loopback_http': 'passed', 'initialize': 'passed', 'tools': 18, 'read_original_crlf': 'passed', 'traversal_denied': 'passed'}, indent=2))
    finally:
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
