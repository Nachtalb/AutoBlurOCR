// Runs the app's own self-test headlessly. No framework, no bundler.
//   node tools/jstest.mjs
import { load } from './pure.mjs';

try {
  load().selfTest();
  console.log('js selfTest: ok');
} catch (e) {
  console.error('js selfTest FAILED:', e.message);
  process.exit(1);
}
