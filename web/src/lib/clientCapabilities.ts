import type { ServerAbout } from "./api";

/**
 * UI capabilities derived from server state. CityHall client mode
 * (`AOE_CITYHALL_MODE`) collapses the dashboard to a composer + structured-view
 * end-user client. Mapping the flag to named capabilities in one place keeps
 * the components free of scattered `serverAbout?.cityhall_mode` checks and
 * documents what the mode actually gates. The server enforces the same
 * restrictions on its endpoints; these flags are UX only. See #7.
 */
export interface ClientCapabilities {
  /** Raw CityHall mode flag, for the rare case a component needs it directly. */
  cityhall: boolean;
  /** Raw Maya restricted-host flag. Server route enforcement remains authoritative. */
  mayaRestricted: boolean;
  /** Terminal view / pane is reachable. */
  canUseTerminal: boolean;
  /** Diff view / pane is reachable. */
  canUseDiff: boolean;
  /** Project add / edit / remove affordances are shown. */
  canManageProjects: boolean;
  /** The new-session wizard collapses to a name-only form. */
  nameOnlyWizard: boolean;
  /** Settings and other deployment-authority surfaces are reachable. */
  canManageDeployment: boolean;
  /** Session workdir, view, agent, and fork authority is reachable. */
  canManageSessionAuthority: boolean;
  /** Background-agent and plugin panes are reachable. */
  canUseExtensions: boolean;
}

export function getClientCapabilities(serverAbout: ServerAbout | null | undefined): ClientCapabilities {
  const cityhall = serverAbout?.cityhall_mode ?? false;
  const mayaRestricted = serverAbout?.maya_restricted ?? false;
  const clientOnly = cityhall || mayaRestricted;
  return {
    cityhall,
    mayaRestricted,
    canUseTerminal: !clientOnly,
    canUseDiff: !clientOnly,
    canManageProjects: !clientOnly,
    nameOnlyWizard: clientOnly,
    canManageDeployment: !mayaRestricted,
    canManageSessionAuthority: !mayaRestricted,
    canUseExtensions: !clientOnly,
  };
}
