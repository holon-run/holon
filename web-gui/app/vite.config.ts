import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { defineConfig, loadEnv } from "vite";
import packageJson from "./package.json";

const DEFAULT_HOLON_API_PROXY_TARGET = "http://127.0.0.1:7878";

export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "");
  const holonApiProxyTarget = env.HOLON_API_PROXY_TARGET || DEFAULT_HOLON_API_PROXY_TARGET;

  return {
    define: {
      __HOLON_GUI_VERSION__: JSON.stringify(packageJson.version),
    },
    plugins: [tailwindcss(), react()],
    server: {
      host: "127.0.0.1",
      port: 5173,
      strictPort: false,
      proxy: {
        "/api": {
          target: holonApiProxyTarget,
          changeOrigin: true,
        },
      },
    },
  };
});
