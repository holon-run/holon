import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, loadEnv } from "vite";

const DEFAULT_HOLON_API_PROXY_TARGET = "http://127.0.0.1:7878";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "");
  const holonApiProxyTarget = env.HOLON_API_PROXY_TARGET || DEFAULT_HOLON_API_PROXY_TARGET;

  return {
    plugins: [tailwindcss(), react()],
    server: {
      host: "127.0.0.1",
      port: 5173,
      strictPort: false,
      proxy: {
        "/holon-api": {
          target: holonApiProxyTarget,
          changeOrigin: true,
          rewrite: (path) => path.replace(/^\/holon-api/, ""),
        },
      },
    },
  };
});
