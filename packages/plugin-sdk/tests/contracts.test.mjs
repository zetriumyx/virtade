import test from 'node:test';import assert from 'node:assert/strict';
import {connectVirt} from '../dist/index.js';
test('UI SDK refuses to assume ambient host authority in the main realm',async()=>{globalThis.window={};window.parent=window;await assert.rejects(connectVirt(),/isolated extension host/);delete globalThis.window;});
