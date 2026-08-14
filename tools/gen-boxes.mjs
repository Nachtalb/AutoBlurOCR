// Headless: ocr.json + a string -> boxes.txt on stdout, using the app's own matching and
// track code. Drives acceptance test 6 (export the fixture, re-OCR, assert zero matches)
// and is handy for scripted batches.
//
//   node tools/gen-boxes.mjs ocr.json "Hauptstrasse 14" [mode] [gapBridgeSec] [padPx] > boxes.txt
import { readFileSync } from 'node:fs';
import { load } from './pure.mjs';

const [file, query, gapBridgeSec = 0.5, padPx = 6] = process.argv.slice(2);
if (!file || query === undefined) {
  console.error('usage: gen-boxes.mjs <ocr.json> <string> [mode] [gapBridgeSec] [padPx]');
  process.exit(2);
}

const P = load();
const ocr = JSON.parse(readFileSync(file, 'utf8'));
const hits = P.matchLines(ocr.lines, query, { exact: true, substr: true, fuzzy: true, regex: '' });
if (!hits.length) {
  console.error('no occurrences matched');
  process.exit(1);
}
const boxes = P.toTracks(hits, {
  rate: ocr.rate, videoFps: ocr.video_fps || ocr.rate, duration: ocr.duration,
  W: ocr.width, H: ocr.height,
  gapBridgeSec: +gapBridgeSec, padPx: +padPx,
});
console.error(`${hits.length} hits -> ${boxes.length} tracks, `
  + `${boxes.reduce((n, b) => n + b.spans.length, 0)} spans`);
process.stdout.write(P.graph(boxes));
