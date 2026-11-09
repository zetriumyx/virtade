import { ClaudeBridge, FrameDecoder, type Frame } from './bridge.js';

const MAX_FRAME = 1024 * 1024;
let bridge: ClaudeBridge;
function send(frame: Frame) {
  const bytes = Buffer.from(`${JSON.stringify(frame)}\n`);
  if (bytes.length > MAX_FRAME || process.stdout.writableLength + bytes.length > 4 * MAX_FRAME) {
    bridge.close();
    process.exitCode = 1;
    process.stdin.destroy();
    throw new Error('outbound frame/queue limit exceeded');
  }
  process.stdout.write(bytes);
}
bridge = new ClaudeBridge(send);
const decoder = new FrameDecoder(MAX_FRAME);
let inFlight = 0;
process.stdin.on('data', (chunk: Buffer) => {
  try {
    for (const frame of decoder.push(chunk)) {
      if (++inFlight > 128) throw new Error('too many concurrent requests');
      void bridge
        .handle(frame)
        .catch(() => {
          bridge.close();
          process.exitCode = 1;
          process.stdin.destroy();
        })
        .finally(() => {
          inFlight--;
        });
    }
  } catch {
    bridge.close();
    process.exitCode = 1;
    process.stdin.destroy();
  }
});
process.stdin.on('end', () => {
  try {
    decoder.end();
  } catch {
    process.exitCode = 1;
  }
  bridge.close();
});
process.stdin.on('error', () => bridge.close());
process.stdout.on('error', () => {
  bridge.close();
  process.stdin.destroy();
});
process.on('SIGTERM', () => {
  bridge.close();
  process.exit(0);
});
process.on('SIGINT', () => {
  bridge.close();
  process.exit(0);
});
