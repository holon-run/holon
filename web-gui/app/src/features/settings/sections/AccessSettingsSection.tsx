import type { RuntimeConnection } from "../../../runtime/types";
import { PairingCard } from "../PairingCard";
import { TailscaleServeCard } from "../TailscaleServeCard";
import type { ServeState } from "../useServeStatus";

interface AccessSettingsSectionProps {
  connection: RuntimeConnection;
  serve: ServeState;
}

export function AccessSettingsSection({ connection, serve }: AccessSettingsSectionProps) {
  return (
    <div className="settings-section-stack">
      <PairingCard connection={connection} serve={serve} />
      <TailscaleServeCard connection={connection} serve={serve} />
    </div>
  );
}
