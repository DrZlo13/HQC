// libheif-js ships no types for its default build: the members the HEIC decoder uses.
declare module 'libheif-js' {
  interface HeifImage {
    get_width(): number;
    get_height(): number;
    /** Writes RGBA pixels into `target.data`; `done` gets `target`, or null on failure. */
    display(target: { data: Uint8ClampedArray; width: number; height: number }, done: (d: object | null) => void): void;
    free(): void;
  }
  const libheif: {
    HeifDecoder: new () => { decode(data: Uint8Array): HeifImage[] };
  };
  export default libheif;
}
