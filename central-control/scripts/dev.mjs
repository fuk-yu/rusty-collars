import { spawn } from "node:child_process";

const server = spawn("npx", ["tsx", "watch", "src/server/main.ts"], {
  stdio: "inherit",
  shell: true,
});

const client = spawn("npx", ["vite", "dev"], {
  stdio: "inherit",
  shell: true,
});

function cleanup() {
  server.kill();
  client.kill();
  process.exit();
}

process.on("SIGINT", cleanup);
process.on("SIGTERM", cleanup);
server.on("exit", cleanup);
client.on("exit", cleanup);
