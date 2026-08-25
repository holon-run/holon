import React from "react";
import ReactDOM from "react-dom/client";

import { I18nProvider } from "./i18n";
import { App } from "./app/App";
import "./styles/tokens.css";
import "./styles/global.css";

if (__HOLON_E2E_DIAGNOSTICS__) {
  void import("./e2e/diagnostics-bridge");
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <I18nProvider>
      <App />
    </I18nProvider>
  </React.StrictMode>,
);
