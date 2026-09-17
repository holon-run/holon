import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Star, ChevronDown } from "lucide-react";
import type { RuntimeModelOption } from "../../runtime/types";
import { modelMatches, modelSourceLabel, providerPresentation } from "../../lib/model-presentation";
import { toggleFavorite, useModelPreferences } from "../../lib/model-preferences";

interface Props {
  options: readonly RuntimeModelOption[];
  value?: string;
  disabled?: boolean;
  autoFocus?: boolean;
  onSelect: (option: RuntimeModelOption) => void;
}

export function ModelList({ options, value, disabled, autoFocus, onSelect }: Props) {
  const { t } = useTranslation();
  const root = useRef<HTMLDivElement>(null);
  const favoriteFocus = useRef<string | null>(null);
  const [limit, setLimit] = useState(60);
  const [query, setQuery] = useState("");
  const [provider, setProvider] = useState("");
  const [showAll, setShowAll] = useState(false);
  const [details, setDetails] = useState<string | null>(null);
  const [storageError, setStorageError] = useState(false);
  const prefs = useModelPreferences();
  useLayoutEffect(() => {
    if (favoriteFocus.current === null) return;
    const target = [...(root.current?.querySelectorAll<HTMLButtonElement>("button[data-favorite-route]") ?? [])]
      .find((button) => button.dataset.favoriteRoute === favoriteFocus.current);
    (target ?? root.current?.querySelector<HTMLInputElement>("input"))?.focus();
    favoriteFocus.current = null;
  }, [prefs]);
  const eligible = useMemo(() => options.filter((option) => option.available || option.routeRef === value), [options, value]);
  const providers = [...new Set(eligible.map((option) => option.routeProvider))]
    .sort((a, b) => providerPresentation(a).label.localeCompare(providerPresentation(b).label));
  const filtered = eligible.filter((option) => (!provider || option.routeProvider === provider) && modelMatches(option, query));
  const byRoute = new Map(filtered.map((option) => [option.routeRef, option]));
  const used = new Set<string>();
  function pick(routes: string[]) {
    return routes.flatMap((route) => {
      const model = byRoute.get(route);
      if (!model || used.has(route)) return [];
      used.add(route);
      return [model];
    });
  }
  const current = pick(value ? [value] : []);
  const favorites = pick(prefs.favorites);
  const recent = pick(prefs.recent);
  const others = filtered.filter((option) => !used.has(option.routeRef)).sort((a, b) => a.displayName.localeCompare(b.displayName) || modelSourceLabel(a).localeCompare(modelSourceLabel(b)) || a.routeRef.localeCompare(b.routeRef));
  const searching = Boolean(query.trim() || provider);
  const hasShortcuts = favorites.length + recent.length > 0;
  const groups = searching ? [{ label: t("modelUi.results"), models: filtered }] : [
    { label: t("modelUi.selected"), models: current },
    { label: t("modelUi.favorites"), models: favorites },
    { label: t("modelUi.recent"), models: recent },
    { label: t("modelUi.allModels"), models: showAll || !hasShortcuts ? others : [] },
  ];
  return <div className="model-browser" ref={root} onKeyDown={(event) => {
    if (event.key === "Enter" && event.target instanceof HTMLInputElement) event.preventDefault();
    if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
    const buttons = [...event.currentTarget.querySelectorAll<HTMLButtonElement>("button[data-model-choice]:not(:disabled)")];
    if (!buttons.length) return;
    if (event.target instanceof HTMLSelectElement) return;
    const index = buttons.indexOf(event.target as HTMLButtonElement);
    if (index < 0 && !(event.target instanceof HTMLInputElement)) return;
    event.preventDefault();
    const next = index < 0 ? (event.key === "ArrowDown" ? 0 : buttons.length - 1)
      : (index + (event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
    buttons[next].focus();
  }}>
    <div className="model-browser-search">
      <input autoFocus={autoFocus} aria-label={t("modelUi.searchModels")} placeholder={t("modelUi.searchModels")}
        value={query} onChange={(event) => { setQuery(event.target.value); setLimit(60); }} />
      <select aria-label={t("modelUi.filterProvider")} value={provider} onChange={(event) => { setProvider(event.target.value); setLimit(60); }}>
        <option value="">{t("modelUi.allServices")}</option>
        {providers.map((key) => <option key={key} value={key}>{providerPresentation(key).label}</option>)}
      </select>
    </div>
    {storageError ? <p role="status" className="settings-hint">{t("modelUi.storageError")}</p> : null}
    <div className="model-browser-results" aria-label={t("modelUi.models")}>
      {value && !options.some((option) => option.routeRef === value) ? <div className="model-browser-missing">
        <strong>{value}</strong><span>{t("modelUi.missingSelected")}</span>
      </div> : null}
      {groups.map(({ label, models }) => models.length > 0 ? <section key={label} aria-label={label}>
        <h3>{label}</h3>
        {models.slice(0, limit).map((model) => <div className={`model-browser-row ${value === model.routeRef ? "is-selected" : ""}`} key={model.routeRef}>
          <button type="button" className="model-browser-choice" data-model-choice disabled={disabled || !model.available}
            aria-pressed={value === model.routeRef} title={model.unavailableReason ?? model.routeRef} onClick={() => onSelect(model)}>
            <strong>{model.displayName}</strong><span>{modelSourceLabel(model)}</span>
            {!model.available ? <small>{model.unavailableReason ?? t("modelUi.unavailable")}</small> : null}
          </button>
          <button type="button" className="model-browser-icon" aria-label={t(prefs.favorites.includes(model.routeRef) ? "modelUi.unfavorite" : "modelUi.favorite", { model: model.displayName, source: modelSourceLabel(model) })}
            data-favorite-route={model.routeRef} aria-pressed={prefs.favorites.includes(model.routeRef)} onClick={() => { favoriteFocus.current = model.routeRef; const saved = toggleFavorite(model.routeRef); setStorageError(!saved); if (!saved) favoriteFocus.current = null; }}>
            <Star size={15} fill={prefs.favorites.includes(model.routeRef) ? "currentColor" : "none"} />
          </button>
          <button type="button" className="model-browser-icon" aria-label={t("modelUi.modelDetails", { model: model.displayName, source: modelSourceLabel(model) })}
            aria-expanded={details === model.routeRef} onClick={() => setDetails(details === model.routeRef ? null : model.routeRef)}><ChevronDown size={15} /></button>
          {details === model.routeRef ? <div className="model-browser-details">
            <label>{t("modelUi.route")}<input readOnly value={model.routeRef} onFocus={(event) => event.currentTarget.select()} /></label>
            <span>{[model.supportsImageInput && t("modelUi.vision"), model.supportsImageGeneration && t("modelUi.imageGeneration"), model.supportsReasoningEffort && t("modelUi.reasoning")].filter(Boolean).join(" · ")}</span>
            {model.availabilityWarning ? <span>{model.availabilityWarning}</span> : null}
          </div> : null}
        </div>)}
      </section> : null)}
      {groups.some((group) => group.models.length > limit) ? <button type="button" className="model-browser-more" onClick={() => setLimit((count) => count + 60)}>{t("modelUi.loadMore")}</button> : null}
      {!filtered.length ? <p className="settings-hint" role="status">{t("modelUi.noModels")}</p> : null}
      {!searching && hasShortcuts && others.length > 0 ? <button type="button" className="model-browser-more" onClick={() => setShowAll(!showAll)} aria-expanded={showAll}>
        {t(showAll ? "modelUi.lessModels" : "modelUi.moreModels", { count: others.length })}
      </button> : null}
    </div>
  </div>;
}
