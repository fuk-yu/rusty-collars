import { mkdir, copyFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import esbuild from "esbuild";

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const rootDir = path.resolve(__dirname, "..");
const distDir = path.join(rootDir, "dist");
const publicDir = path.join(distDir, "public");

await mkdir(publicDir, { recursive: true });

await esbuild.build({
  entryPoints: [path.join(rootDir, "src/index.ts")],
  outfile: path.join(distDir, "index.js"),
  bundle: true,
  platform: "node",
  target: "node22",
  format: "esm",
  sourcemap: true,
  packages: "external",
});

await esbuild.build({
  entryPoints: [path.join(rootDir, "src/client.ts")],
  outfile: path.join(publicDir, "client.js"),
  bundle: true,
  platform: "browser",
  target: "es2022",
  format: "esm",
  sourcemap: true,
});

await copyFile(
  path.join(rootDir, "src/public/index.html"),
  path.join(publicDir, "index.html"),
);
