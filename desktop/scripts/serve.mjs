// Development server: the window in a browser, with the controller and the
// adapters on this machine. For testing only; it is never packaged. It listens
// on localhost only; from another machine, tunnel to it:
//   ssh -L 5180:localhost:5180 <this machine>
// Options: --port N, --simulate [N] (simulated adapters; default 2).
import react from "@vitejs/plugin-react";
import { createServer } from "vite";

const root = new URL("../", import.meta.url).pathname;
const argument = (name) => {
  const i = process.argv.indexOf(name);
  return i < 0 ? null : (process.argv[i + 1] ?? "");
};
const port = Number(argument("--port") ?? 5180);
const simulateArg = argument("--simulate");
const simulate = simulateArg === null ? Number(process.env.CORDIAL_DESKTOP_SIMULATE ?? 0) : Number(simulateArg) || 2;

let backend;
const server = await createServer({
  configFile: false,
  root: `${root}src/server`,
  plugins: [
    react(),
    {
      name: "cordial-backend",
      configureServer(server) {
        server.middlewares.use((req, res, next) => {
          if (!backend?.handle(req, res)) next();
        });
      },
    },
  ],
  server: { host: "localhost", port, strictPort: true, fs: { allow: [root] } },
});
backend = await (await server.ssrLoadModule(`${root}src/server/backend.ts`)).startBackend(simulate);
await server.listen();
server.printUrls();

let stopping = false;
for (const signal of ["SIGINT", "SIGTERM"])
  process.on(signal, async () => {
    if (stopping) return;
    stopping = true;
    await Promise.race([backend.stop(), new Promise((r) => setTimeout(r, 1500))]);
    await server.close();
    process.exit(0);
  });
