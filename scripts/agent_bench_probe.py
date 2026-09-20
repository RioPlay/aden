#!/usr/bin/env python3
# Copyright (c) 2026 RioPlay <rioplay@rioplay.dev>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Local stdio observation adapter; not a model runner or a security sandbox.

Read a prepared run manifest and record actual requests/results for replay by
agent_bench.telemetry_from_events. The operator owns the manifest and log paths.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import threading
import time
import uuid


class ProtocolError(RuntimeError):
    """The subprocess responded, but did not provide a usable JSON-RPC message."""


def source_range(project: Path, arguments: dict) -> str:
    path = (project / arguments['file']).resolve()
    if not path.is_relative_to(project.resolve()):
        raise ValueError('source range must remain inside the assigned project')
    start, end = int(arguments['start']), int(arguments['end'])
    if start < 1 or end < start or end - start >= 160:
        raise ValueError('source reads require a 1-based range of at most 160 lines')
    lines = path.read_text(encoding='utf-8').splitlines()
    return '\n'.join(f'{n}: {line}' for n, line in enumerate(lines[start-1:end], start))


def mcp_request(manifest: dict, run: dict, tool: str, arguments: dict) -> dict:
    env = dict(os.environ, ADEN_BIN=manifest['binary']['aden.exe']['path'],
               ADEN_DATA_DIR=run['data'], ADEN_MCP_SURFACE='essential')
    # Avoid an inherited developer setting changing the frozen trial condition.
    if run.get('skip_auto_gen'):
        env['ADEN_SKIP_AUTO_GEN'] = '1'
    else:
        env.pop('ADEN_SKIP_AUTO_GEN', None)
    proc = subprocess.Popen([manifest['binary']['aden-mcp.exe']['path'], run['project']],
                            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                            text=True, encoding='utf-8', env=env)
    messages: queue.Queue = queue.Queue()
    diagnostics = []
    def stdout_pump():
        try:
            for line in proc.stdout:
                messages.put(json.loads(line))
        except Exception as error:
            messages.put(error)
        finally:
            messages.put(None)
    def stderr_pump():
        for line in proc.stderr:
            diagnostics.append(line)
    threading.Thread(target=stdout_pump, daemon=True).start()
    threading.Thread(target=stderr_pump, daemon=True).start()
    def send(message):
        proc.stdin.write(json.dumps(message) + '\n')
        proc.stdin.flush()
    deadline = time.monotonic() + 90
    def receive(request_id):
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError(f'MCP deadline expired waiting for response {request_id}')
            try:
                value = messages.get(timeout=remaining)
            except queue.Empty as error:
                raise TimeoutError(f'MCP deadline expired waiting for response {request_id}') from error
            if value is None:
                raise RuntimeError('MCP process closed stdout: ' + ''.join(diagnostics)[-1000:])
            if isinstance(value, Exception):
                raise ProtocolError(str(value)) from value
            if not isinstance(value, dict):
                raise ProtocolError('MCP message must be a JSON object')
            if value.get('id') == request_id:
                if 'result' not in value and 'error' not in value:
                    raise ProtocolError('MCP response has neither result nor error')
                return value
    try:
        send({'jsonrpc':'2.0', 'id':1, 'method':'initialize', 'params':{
            'protocolVersion':'2025-03-26', 'capabilities':{},
            'clientInfo':{'name':'aden-agent-usage-observer','version':'1'}}})
        initialized = receive(1)
        if 'error' in initialized:
            return initialized
        send({'jsonrpc':'2.0','method':'notifications/initialized'})
        if tool == 'describe':
            send({'jsonrpc':'2.0','id':2,'method':'tools/list','params':{}})
            listed = receive(2)
            if 'error' in listed:
                return listed
            return {'result':{'initialize':initialized['result'],'tools':listed['result']}}
        send({'jsonrpc':'2.0','id':3,'method':'tools/call',
              'params':{'name':tool,'arguments':arguments}})
        return receive(3)
    finally:
        proc.kill()
        proc.wait()


def observe(config: Path, run_id: str, tool: str, arguments: dict, log_name: str,
            argument_error: str | None = None) -> dict:
    manifest = json.loads(config.read_text(encoding='utf-8'))
    run = manifest['runs'][run_id]
    project = Path(run['project'])
    log = project.parent / log_name
    if Path(log_name).name != log_name:
        raise ValueError('log name must be a filename')
    item = {'id':str(uuid.uuid4()), 'type':'mcp_tool_call', 'server':'aden',
            'tool':tool, 'arguments':arguments}
    if tool in {'source', 'tests'} or argument_error:
        item.update(type='command_execution', command=f'{tool} {json.dumps(arguments, sort_keys=True)}')
    if argument_error:
        item['command'] = f'observer argument validation for {tool}'
    def record(kind, value):
        with log.open('a', encoding='utf-8') as out:
            out.write(json.dumps({'type':kind, 'recorded_at':datetime.now(timezone.utc).isoformat(),
                                  'item':value}, ensure_ascii=True) + '\n')
    record('item.started', item)
    started = time.monotonic()
    completed = dict(item, status='completed')
    try:
        if argument_error:
            raise ValueError(argument_error)
        if tool == 'source':
            text = source_range(project, arguments)
            completed.update(exit_code=0, aggregated_output=text)
            output = {'source':arguments, 'text':text}
        elif tool == 'tests':
            if run['scenario'] != 'coding' or arguments:
                raise ValueError('tests is available only for coding cases and takes no arguments')
            result = subprocess.run(['cargo','test','--locked'], cwd=project,
                                    capture_output=True, text=True, encoding='utf-8', timeout=120)
            completed.update(exit_code=result.returncode, aggregated_output=result.stdout + result.stderr)
            output = {'exit_code':result.returncode, 'output':result.stdout + result.stderr}
        else:
            output = mcp_request(manifest, run, tool, arguments)
            if 'error' in output:
                completed.update(status='failed', error={'kind':'protocol', 'message':output['error']})
            else:
                completed['result'] = output['result']
    except Exception as error:
        kind = 'protocol' if isinstance(error, ProtocolError) else 'transport' if item['type']=='mcp_tool_call' else 'shell'
        completed.update(status='failed', error={'kind':kind,
                                                 'message':str(error)})
        output = {'observer_error':str(error)}
    completed['elapsed_ms'] = round((time.monotonic() - started) * 1000)
    record('item.completed', completed)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--run', required=True)
    parser.add_argument('--tool', required=True)
    parser.add_argument('--arg', action='append', default=[], metavar='KEY=VALUE')
    parser.add_argument('--log', default='events.jsonl')
    args = parser.parse_args()
    arguments = {}
    argument_error = None
    try:
        for pair in args.arg:
            key, value = pair.split('=', 1)
            if not key or key in arguments:
                raise ValueError('argument keys must be nonempty and unique')
            try:
                arguments[key] = json.loads(value)
            except json.JSONDecodeError:
                arguments[key] = value
    except ValueError as error:
        argument_error = f'invalid KEY=VALUE arguments: {error}'
    result = observe(args.config, args.run, args.tool, arguments, args.log, argument_error)
    print(json.dumps(result, ensure_ascii=True))
    if 'observer_error' in result:
        sys.exit(1)


if __name__ == '__main__':
    main()
