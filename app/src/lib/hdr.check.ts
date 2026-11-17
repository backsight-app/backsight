// Self-check for hdr.ts. The app has no test runner, so run it directly:
//   node --experimental-strip-types src/lib/hdr.check.ts   (from app/)
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fitHdr, hdrFits, hdrPossible, listed } from "./hdr.ts";

const catalog = JSON.parse(readFileSync(new URL("../../../content/encoders.json", import.meta.url), "utf8"));
const video = (o: object) => ({ capture_target: "monitor", encoder: "nvenc-hevc", container: "mp4-fragmented", ...o });

// Typical NVIDIA settings already fit: nothing changes, nothing to say.
let v = video({});
assert.deepEqual(fitHdr(v, catalog, {}), []);
assert.deepEqual(v, video({}));
assert.ok(hdrFits(v, catalog));

// Another encoder and a file format without HDR are fixed in one go; Game window only stays (it records HDR too).
v = video({ capture_target: "window", encoder: "nvenc-h264", container: "mov" });
assert.ok(!hdrFits(v, catalog));
assert.deepEqual(fitHdr(v, catalog, {}), ["NVIDIA NVENC HEVC", "MP4"]);
assert.ok(hdrFits(v, catalog));
assert.equal(v.capture_target, "window");

// A crash-safe file format stays crash-safe; one that already fits is kept.
v = video({ container: "mpegts" });
if (catalog.containers.some((c: any) => c.id === "mpegts")) {
  fitHdr(v, catalog, {});
  assert.ok(catalog.containers.find((c: any) => c.id === v.container).crashSafe, v.container);
}
v = video({ container: "mkv" });
assert.deepEqual(fitHdr(v, catalog, {}), []);
assert.equal(v.container, "mkv");

// An AMD or CPU encoder still ends on an HDR encoder that works here.
v = video({ encoder: "x264" });
fitHdr(v, catalog, {});
assert.ok(hdrFits(v, catalog));

// No HDR encoder works on this PC: say so, change nothing.
const broken = Object.fromEntries(catalog.videoEncoders.filter((e: any) => e.hdr).map((e: any) => [e.id, "Needs a driver."]));
v = video({ capture_target: "window", encoder: "x264" });
assert.equal(fitHdr(v, catalog, broken), null);
assert.deepEqual(v, video({ capture_target: "window", encoder: "x264" }));
assert.ok(!hdrPossible(catalog, broken));
assert.ok(hdrPossible(catalog, {}));

// With a second HDR encoder in the catalog, a broken one is skipped and the same kind of video is preferred.
const two = {
  ...catalog,
  videoEncoders: [
    ...catalog.videoEncoders,
    { id: "other-hevc", label: "Other HEVC", codec: "hevc", vendor: "amd", hdr: true },
    { id: "other-av1", label: "Other AV1", codec: "av1", vendor: "nvidia", hdr: true },
  ],
};
v = video({});
assert.deepEqual(fitHdr(v, two, { "nvenc-hevc": "Needs a driver." }), ["Other HEVC"]);
v = video({ encoder: "nvenc-av1" });
assert.deepEqual(fitHdr(v, two, {}), ["Other AV1"]);

assert.equal(listed([]), "");
assert.equal(listed(["A"]), "A");
assert.equal(listed(["A", "B"]), "A and B");
assert.equal(listed(["A", "B", "C"]), "A, B and C");

console.log("hdr.check: ok");
