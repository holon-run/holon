import settings from "./mdorigin.config.json" with { type: "json" };
import navigation from "./assets/docs-navigation.json" with { type: "json" };
import { documentationLayout } from "./.theme/docs-layout.mjs";
import { structuredDataLayout } from "./.theme/structured-data.mjs";

// All CLI commands use .tools/dist/content, where prepare:content builds navigation.
export default { ...settings, plugins: [documentationLayout(navigation), structuredDataLayout(navigation)] };
