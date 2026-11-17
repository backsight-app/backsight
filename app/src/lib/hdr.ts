// What turning HDR video on changes, so it works without a trip through the other settings.
// Pure: the catalog and this PC's encoder check results come in as arguments (see hdr.check.ts).

const holdsHdr = (box: any, codec: string) => ["mp4", "mkv"].includes(box?.ext) && box.videoCodecs.includes(codec);

/** Whether these video settings record HDR as they are (the engine records standard video otherwise). */
export function hdrFits(video: any, catalog: any): boolean {
  const enc = catalog.videoEncoders.find((e: any) => e.id === video.encoder);
  const box = catalog.containers.find((c: any) => c.id === video.container);
  return !!enc?.hdr && holdsHdr(box, enc.codec);
}

/** Whether any HDR encoder passed the encoder check on this PC (`broken`: encoder id → why it failed). */
export const hdrPossible = (catalog: any, broken: Record<string, string>) =>
  catalog.videoEncoders.some((e: any) => e.hdr && !(e.id in broken));

/**
 * Switches `video` to what HDR needs on this PC: an HDR encoder that works
 * here (the same kind of video, then the same graphics card maker, when there is a choice) and an
 * MP4 or MKV file (crash-safe stays crash-safe). Returns the labels of what it switched to, or
 * `null`, with nothing changed, when no HDR encoder works here.
 */
export function fitHdr(video: any, catalog: any, broken: Record<string, string>): string[] | null {
  const working = catalog.videoEncoders.filter((e: any) => e.hdr && !(e.id in broken));
  const now = catalog.videoEncoders.find((e: any) => e.id === video.encoder);
  const enc =
    working.find((e: any) => e.id === now?.id) ??
    working.find((e: any) => e.codec === now?.codec) ??
    working.find((e: any) => e.vendor === now?.vendor) ??
    working[0];
  if (!enc) return null;
  const changed: string[] = [];
  if (video.encoder !== enc.id) {
    video.encoder = enc.id;
    changed.push(enc.label);
  }
  const box = catalog.containers.find((c: any) => c.id === video.container);
  if (!holdsHdr(box, enc.codec)) {
    const fits = catalog.containers.filter((c: any) => holdsHdr(c, enc.codec));
    const next = fits.find((c: any) => c.crashSafe === !!box?.crashSafe) ?? fits[0];
    // An HDR encoder no MP4 or MKV file can hold would be a catalog mistake; the engine then records standard video.
    if (next) {
      video.container = next.id;
      changed.push(next.label);
    }
  }
  return changed;
}

/** "A", "A and B", "A, B and C". */
export const listed = (names: string[]) =>
  names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names.at(-1)}`;
