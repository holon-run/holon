// Compact vivid artwork. Identity mapping deliberately retains the V8 seed namespace.
export const AGENT_AVATAR_VERSION = "light-mech-1-vivid-2";
const SEED_VERSION = "light-mech-1";

export type AgentAvatarExpression = "neutral" | "focused" | "questioning" | "waiting" | "sleeping";

const palettes = [
  ["#ff9826", "#d96a10", "#faecdc"], ["#28a9ef", "#1279be", "#e4f0f7"],
  ["#36cf9e", "#189b72", "#e4f1e9"], ["#8561df", "#5c3bb1", "#eee9f8"],
  ["#ffd23d", "#d5a019", "#f7f0d8"], ["#ff6c65", "#d5424c", "#f9e8e3"],
  ["#17bec2", "#0c8d97", "#e1f0f1"], ["#eb63aa", "#ba3d80", "#f5e7ef"],
  ["#94c94a", "#649529", "#edf1df"], ["#547be4", "#3554b0", "#e8edf9"],
  ["#df8147", "#b55428", "#f4e9e0"], ["#668fac", "#426580", "#eaf0f3"],
] as const;

const shells = [
  {
    head: "M29 27H71L81 37V73L71 83H29L19 73V37Z",
    side: "M12 45H19V65H12Q8 65 8 61V49Q8 45 12 45Z M81 45H88Q92 45 92 49V61Q92 65 88 65H81Z",
    shade: "M19 67L29 77H71L81 67V73L71 83H29L19 73Z",
    face: "M33 37H67Q73 37 73 43V64Q73 71 66 71H34Q27 71 27 64V43Q27 37 33 37Z",
    eyes: [50, 54, 10], anchor: [50, 27]
  },
  {
    head: "M31 34H69Q90 34 90 56T69 79H31Q10 79 10 57T31 34Z",
    side: "M7 50H13V63H7Q4 63 4 59V54Q4 50 7 50Z M87 50H93Q96 50 96 54V59Q96 63 93 63H87Z",
    shade: "M13 65Q19 73 32 73H68Q81 73 87 65Q83 79 69 79H31Q17 79 13 65Z",
    face: "M31 43H69Q80 43 80 56T69 69H31Q20 69 20 56T31 43Z",
    eyes: [50, 56, 12], anchor: [50, 34]
  },
  {
    head: "M39 20H61Q74 20 74 33V74Q74 86 62 86H38Q26 86 26 74V33Q26 20 39 20Z",
    side: "M20 58H26V74H20Q16 74 16 69V63Q16 58 20 58Z M74 58H80Q84 58 84 63V69Q84 74 80 74H74Z",
    shade: "M26 68Q27 79 39 79H61Q73 79 74 68V74Q74 86 62 86H38Q26 86 26 74Z",
    face: "M40 34H60Q66 34 66 41V66Q66 73 59 73H41Q34 73 34 66V41Q34 34 40 34Z",
    eyes: [50, 53, 8], anchor: [50, 20]
  },
  {
    head: "M32 27H68L87 55L70 83H30L13 55Z",
    side: "M12 47L18 47L18 63L12 63L7 55Z M82 47H88L93 55L88 63H82Z",
    shade: "M20 65L34 77H66L80 65L70 83H30Z",
    face: "M35 39H65L75 55L65 70H35L25 55Z",
    eyes: [50, 54, 10], anchor: [50, 27]
  },
  {
    head: "M19 30H81Q88 30 85 39L75 74Q73 82 65 82H35Q27 82 25 74L15 39Q12 30 19 30Z",
    side: "M10 37L18 39L23 55L12 52Q7 51 7 46V41Q7 37 10 37Z M90 37L82 39L77 55L88 52Q93 51 93 46V41Q93 37 90 37Z",
    shade: "M23 67L34 76H66L77 67L75 74Q73 82 65 82H35Q27 82 25 74Z",
    face: "M30 41H70Q75 41 73 47L68 64Q67 70 61 70H39Q33 70 32 64L27 47Q25 41 30 41Z",
    eyes: [50, 54, 11], anchor: [50, 30]
  },
  {
    head: "M17 60Q17 24 50 24Q83 24 83 60V72Q83 82 73 82H27Q17 82 17 72Z",
    side: "M12 56H18V74H12Q8 74 8 68V62Q8 56 12 56Z M82 56H88Q92 56 92 62V68Q92 74 88 74H82Z",
    shade: "M17 68Q19 76 28 76H72Q81 76 83 68V72Q83 82 73 82H27Q17 82 17 72Z",
    face: "M27 56Q27 40 50 40Q73 40 73 56V65Q73 71 67 71H33Q27 71 27 65Z",
    eyes: [50, 56, 10], anchor: [50, 25]
  },
  {
    head: "M40 25H60Q67 25 71 36L83 62Q89 80 72 85Q50 92 28 85Q11 80 17 62L29 36Q33 25 40 25Z",
    side: "M17 59H22V73H17Q13 73 13 69V63Q13 59 17 59Z M78 59H83Q87 59 87 63V69Q87 73 83 73H78Z",
    shade: "M16 71Q24 81 50 81Q76 81 84 71Q84 82 72 85Q50 92 28 85Q16 82 16 71Z",
    face: "M39 38H61Q66 38 68 45L74 62Q78 74 66 76H34Q22 74 26 62L32 45Q34 38 39 38Z",
    eyes: [50, 57, 10], anchor: [50, 25]
  },
  {
    head: "M27 27H57V34H73L82 43V73L72 83H28L18 73V37Z",
    side: "M9 46H18V65H9Q6 65 6 61V50Q6 46 9 46Z M82 47H90Q94 47 94 51V68Q94 72 90 72H82Z",
    shade: "M18 67L28 77H72L82 67V73L72 83H28L18 73Z",
    face: "M32 41H65Q73 41 73 49V63Q73 71 65 71H33Q27 71 27 65V47Q27 41 32 41Z",
    eyes: [50, 56, 10], anchor: [41, 27]
  }
] as const;

const CORE = shells.length * palettes.length;
// Retain the original index space, including its former expression slots.
const TOTAL = CORE * 4 * 3;
const INK = "#243442";
const SCREEN = "#fffdf5";

export function agentAvatarIndex(agentId: string): number {
  let hash = 2166136261;
  // Iterate Unicode code points, matching the reviewed V8 generator.
  for (const char of `${SEED_VERSION}:${agentId}`) {
    hash = Math.imul(hash ^ char.codePointAt(0)!, 16777619);
  }
  return (hash >>> 0) % TOTAL;
}

function path(d: string, fill: string, stroke = "none", width = 3): string {
  return `<path d="${d}" fill="${fill}" stroke="${stroke}" stroke-width="${width}"/>`;
}

/** Neutral by default, matching the UI and exported brand artwork. */
export function renderAgentAvatar(agentId: string, stateExpression: AgentAvatarExpression = "neutral"): string {
  const index = agentAvatarIndex(agentId);
  const shell = shells[index % shells.length];
  const [body, shade, background] = palettes[Math.floor(index / shells.length) % palettes.length];
  const crown = Math.floor(index / (CORE * 4)) % 3;
  const [cx, cy] = shell.anchor;
  let art: string;
  if (crown === 0) {
    art = path(`M${cx - 8} ${cy + 1}V${cy - 5}H${cx + 8}V${cy + 1}Z`, shade, INK);
  } else if (crown === 1) {
    art = path(`M${cx} ${cy}V${cy - 9}`, "none", INK, 4)
      + `<rect x="${cx - 5}" y="${cy - 15}" width="10" height="8" rx="3" fill="${body}" stroke="${INK}" stroke-width="3"/>`;
  } else {
    art = path(`M${cx - 12} ${cy + 1}V${cy - 7}Q${cx - 12} ${cy - 10} ${cx - 8} ${cy - 10}H${cx + 8}Q${cx + 12} ${cy - 10} ${cx + 12} ${cy - 7}V${cy + 1}`, "none", INK, 5);
  }
  art += path(shell.side, shade, INK) + path(shell.head, body, INK)
    + path(shell.shade, shade) + path(shell.face, SCREEN, INK, 2.8);
  const [x, y, gap] = shell.eyes;
  art += `<g data-avatar-part="expression">${renderStatusEyes(stateExpression, x, y, gap)}</g>`;
  return `<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100" viewBox="0 0 100 100"><rect width="100" height="100" fill="${background}"/><g stroke-linecap="round" stroke-linejoin="round">${art}</g></svg>`;
}

function renderStatusEyes(expression: AgentAvatarExpression, x: number, y: number, gap: number): string {
  const left = x - gap;
  const right = x + gap;
  switch (expression) {
    case "focused":
      return path(`M${left - 4} ${y - 5}H${left + 4} M${right - 4} ${y - 5}H${right + 4}`, "none", INK, 3)
        + `<rect x="${left - 3}" y="${y - 1}" width="6" height="4" rx="1.5" fill="${INK}"/><rect x="${right - 3}" y="${y - 1}" width="6" height="4" rx="1.5" fill="${INK}"/>`;
    case "questioning":
      return `<circle cx="${left}" cy="${y + 1}" r="3" fill="${INK}"/><rect x="${right - 3}" y="${y - 5}" width="6" height="10" rx="3" fill="${INK}"/>`
        + path(`M${right - 4} ${y - 9}Q${right} ${y - 12} ${right + 4} ${y - 9}`, "none", INK, 2.5);
    case "waiting":
      return `<circle cx="${left + 2}" cy="${y}" r="3.5" fill="${INK}"/><circle cx="${right + 2}" cy="${y}" r="3.5" fill="${INK}"/>`;
    case "sleeping":
      return path(`M${left - 4} ${y}Q${left} ${y + 4} ${left + 4} ${y} M${right - 4} ${y}Q${right} ${y + 4} ${right + 4} ${y}`, "none", INK, 3.5);
    default:
      // Unknown inputs are neutral too; never interpolate caller text into SVG.
      return `<rect x="${left - 2.7}" y="${y - 4.5}" width="5.4" height="9" rx="2.7" fill="${INK}"/><rect x="${right - 2.7}" y="${y - 4.5}" width="5.4" height="9" rx="2.7" fill="${INK}"/>`;
  }
}
