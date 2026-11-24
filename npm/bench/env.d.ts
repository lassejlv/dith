// Aliases set in bench/vite.config.ts (an optional older build to compare against).
declare module "dith-baseline" {
  export const missing: boolean | undefined;
}
declare module "dith-baseline-wasm?url" {
  const url: string;
  export default url;
}
