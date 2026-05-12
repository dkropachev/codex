import { defineConfig } from "tsup";

export default defineConfig({
  entry: ["src/index.ts", "src/workflow.ts"],
  format: ["esm"],
  dts: true,
  sourcemap: true,
  clean: true,
  minify: false,
  target: "node18",
  shims: false,
});
