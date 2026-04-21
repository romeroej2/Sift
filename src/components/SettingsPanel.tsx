import type { BrowserSource, LmStudioHealth, ScheduleRule, UserSettings } from "../lib/types";
import { createScheduleRule, DEFAULT_SHORT_BROWSE_PAGE_COUNT } from "../lib/defaults";
import {
  getLmStudioSummary,
  getModelDeskStatusLabel,
  getModelDeskSummary
} from "../lib/app-utils";

function formatRelativeSavedAt(savedAt: number, now: number) {
  const delta = Math.max(0, now - savedAt);
  const seconds = Math.floor(delta / 1000);
  if (seconds < 60) {
    return "just now";
  }

  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) {
    return `${minutes} min ago`;
  }

  const hours = Math.floor(minutes / 60);
  return `${hours} hr ago`;
}

function SettingsSaveIndicator({
  isDirty,
  hasEnabledSources,
  lastSavedAt,
  now
}: {
  isDirty: boolean;
  hasEnabledSources: boolean;
  lastSavedAt: number | null;
  now: number;
}) {
  let tone: "muted" | "saving" | "saved" | "warning" = "muted";
  let label = "Autosave enabled";

  if (isDirty && !hasEnabledSources) {
    tone = "warning";
    label = "Enable a source to autosave";
  } else if (isDirty) {
    tone = "saving";
    label = "Saving\u2026";
  } else if (lastSavedAt !== null) {
    tone = "saved";
    label = `Saved \u00b7 ${formatRelativeSavedAt(lastSavedAt, now)}`;
  }

  return (
    <div
      className={`settings-save-indicator settings-save-indicator--${tone}`}
      role="status"
      aria-live="polite"
    >
      <span className="settings-save-indicator__dot" aria-hidden="true" />
      <span>{label}</span>
    </div>
  );
}

export function SettingsPanel({
  settings,
  scheduleSummary,
  isModelDeskExpanded,
  setIsModelDeskExpanded,
  lmStudioDraft,
  setLmStudioDraft,
  lmHealth,
  selectedModelId,
  availableModels,
  onVerifyLmStudio,
  onSaveModelDesk,
  isSettingsDirty,
  lastSettingsSavedAt,
  now,
  onChange
}: {
  settings: UserSettings;
  scheduleSummary: { title: string; detail: string };
  isModelDeskExpanded: boolean;
  setIsModelDeskExpanded: React.Dispatch<React.SetStateAction<boolean>>;
  lmStudioDraft: UserSettings["lmStudio"];
  setLmStudioDraft: React.Dispatch<React.SetStateAction<UserSettings["lmStudio"]>>;
  lmHealth: LmStudioHealth | null;
  selectedModelId: string | null;
  availableModels: LmStudioHealth["models"];
  onVerifyLmStudio: () => void;
  onSaveModelDesk: () => void;
  isSettingsDirty: boolean;
  lastSettingsSavedAt: number | null;
  now: number;
  onChange: (value: UserSettings) => void;
}) {
  const hasEnabledSources =
    settings.capture.sources.x
    || settings.capture.sources.linkedin
    || settings.capture.sources.reddit;
  const updateScheduleRule = (ruleId: string, updater: (rule: ScheduleRule) => ScheduleRule) =>
    onChange({
      ...settings,
      schedule: {
        ...settings.schedule,
        rules: settings.schedule.rules.map((rule) => (rule.id === ruleId ? updater(rule) : rule))
      }
    });
  const addScheduleRule = () =>
    onChange({
      ...settings,
      schedule: {
        ...settings.schedule,
        rules: [
          ...settings.schedule.rules,
          createScheduleRule({
            label: `Schedule ${settings.schedule.rules.length + 1}`,
            cadence: "interval",
            browsePageCount: DEFAULT_SHORT_BROWSE_PAGE_COUNT
          })
        ]
      }
    });
  const removeScheduleRule = (ruleId: string) =>
    onChange({
      ...settings,
      schedule: {
        ...settings.schedule,
        rules: settings.schedule.rules.filter((rule) => rule.id !== ruleId)
      }
    });

  return (
    <section className="panel content-panel">
      <div className="section-header section-header--split">
        <div>
          <p className="kicker">Settings</p>
          <h2>Shape the paper.</h2>
        </div>
        <SettingsSaveIndicator
          isDirty={isSettingsDirty}
          hasEnabledSources={Boolean(hasEnabledSources)}
          lastSavedAt={lastSettingsSavedAt}
          now={now}
        />
      </div>

      <div className="settings-stack">
        <section className="settings-card">
          <div className="settings-card__header">
            <div>
              <p className="kicker">Models</p>
              <h3>Model desk</h3>
            </div>
            <p className="settings-card__copy">
              Verify the local LM Studio endpoint, choose the active model, and decide whether image attachments should be sent during ranking.
            </p>
          </div>

          <section className="model-desk model-desk--settings">
            <button
              className={
                isModelDeskExpanded
                  ? "model-desk__summary model-desk__summary--open"
                  : "model-desk__summary"
              }
              onClick={() => setIsModelDeskExpanded((current) => !current)}
              type="button"
            >
              <span className="model-desk__summary-main">
                <span className="model-desk__icon" aria-hidden="true">
                  <span />
                  <span />
                  <span />
                </span>
                <span className="model-desk__summary-copy">
                  <strong>Model desk</strong>
                  <span>{getModelDeskSummary(selectedModelId, lmHealth)}</span>
                </span>
              </span>
              <span className="model-desk__summary-meta">
                <span className={lmHealth ? "status-badge status-badge--ready" : "status-badge"}>
                  {getModelDeskStatusLabel(selectedModelId, lmHealth)}
                </span>
                <span className="model-desk__chevron" aria-hidden="true">
                  {isModelDeskExpanded ? "\u2212" : "+"}
                </span>
              </span>
            </button>

            {isModelDeskExpanded ? (
              <div className="model-desk__panel">
                <div className="model-desk__group">
                  <label className="field">
                    <span>LM Studio URL</span>
                    <input
                      value={lmStudioDraft.baseUrl}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          baseUrl: event.target.value
                        }))
                      }
                    />
                  </label>
                  <label className="field">
                    <span>Auth token</span>
                    <input
                      type="password"
                      value={lmStudioDraft.authToken ?? ""}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          authToken: event.target.value || null
                        }))
                      }
                      placeholder="Optional"
                    />
                    <small>Optional. Kept only for the current app session.</small>
                  </label>
                  <div className="button-row model-desk__actions">
                    <button className="primary-button" onClick={onVerifyLmStudio}>
                      Update
                    </button>
                    <button className="secondary-button" onClick={onSaveModelDesk}>
                      Save
                    </button>
                  </div>
                </div>

                <div className="model-desk__group">
                  <label className="field">
                    <span>Selected model</span>
                    <select
                      value={lmStudioDraft.selectedModel ?? ""}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          selectedModel: event.target.value || null
                        }))
                      }
                      disabled={!availableModels.length}
                    >
                      <option value="">
                        {availableModels.length ? "Pick a local model" : "Verify LM Studio first"}
                      </option>
                      {availableModels.map((model) => (
                        <option key={model.id} value={model.id}>
                          {model.id}
                        </option>
                      ))}
                    </select>
                  </label>

                  <div className={lmHealth ? "model-status model-status--verified" : "model-status"}>
                    <strong>
                      {lmHealth
                        ? "LM Studio verified"
                        : selectedModelId
                          ? "Saved model restored"
                          : "Not verified yet"}
                    </strong>
                    <span>
                      {lmHealth
                        ? getLmStudioSummary(lmHealth)
                        : selectedModelId
                          ? "SIFT restored your saved LM Studio selection. Verify to refresh the live model list."
                          : "Verify the connection to load local models."}
                    </span>
                    {selectedModelId ? (
                      <span className="model-status__selected">
                        Active: <strong>{selectedModelId}</strong>
                      </span>
                    ) : null}
                  </div>

                  <label className="field field--checkbox">
                    <input
                      type="checkbox"
                      checked={lmStudioDraft.includeImages}
                      onChange={(event) =>
                        setLmStudioDraft((current) => ({
                          ...current,
                          includeImages: event.target.checked
                        }))
                      }
                    />
                    <span>Use attached post images during ranking</span>
                  </label>
                  <p className="field-help">
                    Enable this only for vision-capable local models. SIFT will download attached post photos and send them to LM Studio when ranking digest topics.
                  </p>
                </div>
              </div>
            ) : null}
          </section>
        </section>

        <section className="settings-card">
          <div className="settings-card__header">
            <div>
              <p className="kicker">Capture</p>
              <h3>Source desk</h3>
            </div>
            <p className="settings-card__copy">
              Choose where the paper should pull from. Browse depth now lives inside each schedule rule so shorter auto-runs can stay lighter than the daily brief.
            </p>
          </div>

          <div className="settings-source-grid">
            <section
              className={`settings-source-tile${settings.capture.sources.x ? " settings-source-tile--enabled settings-source-tile--x" : ""}`}
            >
              <div className="settings-source-tile__top">
                <div>
                  <span className="settings-source-tile__title">X</span>
                  <span className="settings-source-tile__eyebrow">Short-form pulse</span>
                </div>
                <input
                  type="checkbox"
                  checked={settings.capture.sources.x}
                  onChange={(event) =>
                    onChange({
                      ...settings,
                      capture: {
                        ...settings.capture,
                        sources: {
                          ...settings.capture.sources,
                          x: event.target.checked
                        }
                      }
                    })
                  }
                />
              </div>
              <p className="settings-source-tile__copy">
                Fast, denser posts. Good when you want more breadth and chatter.
              </p>
            </section>

            <section
              className={`settings-source-tile${settings.capture.sources.linkedin ? " settings-source-tile--enabled settings-source-tile--linkedin" : ""}`}
            >
              <div className="settings-source-tile__top">
                <div>
                  <span className="settings-source-tile__title">LinkedIn</span>
                  <span className="settings-source-tile__eyebrow">Long-form signal</span>
                </div>
                <input
                  type="checkbox"
                  checked={settings.capture.sources.linkedin}
                  onChange={(event) =>
                    onChange({
                      ...settings,
                      capture: {
                        ...settings.capture,
                        sources: {
                          ...settings.capture.sources,
                          linkedin: event.target.checked
                        }
                      }
                    })
                  }
                />
              </div>
              <p className="settings-source-tile__copy">
                Larger, slower cards. Tune this separately when you want fewer but heavier LinkedIn pages.
              </p>
            </section>

            <section
              className={`settings-source-tile${settings.capture.sources.reddit ? " settings-source-tile--enabled settings-source-tile--reddit" : ""}`}
            >
              <div className="settings-source-tile__top">
                <div>
                  <span className="settings-source-tile__title-row">
                    <span
                      className="settings-source-tile__source-icon settings-source-tile__source-icon--reddit"
                      aria-hidden="true"
                    >
                      <svg viewBox="0 0 24 24" fill="none">
                        <circle cx="12" cy="13" r="5.5" />
                        <path d="M9 18c.7.5 1.8.8 3 .8s2.3-.3 3-.8" />
                        <circle cx="9.8" cy="13" r=".9" fill="currentColor" stroke="none" />
                        <circle cx="14.2" cy="13" r=".9" fill="currentColor" stroke="none" />
                        <path d="M10.5 7.2 12.2 9" />
                        <circle cx="15.8" cy="6.8" r="1.2" />
                        <path d="M7.8 10.2c-.8-.5-1.5-1.2-1.5-2.1 0-1 1-1.8 2.3-1.8.7 0 1.3.2 1.8.5" />
                        <path d="M16.2 10.2c.8-.5 1.5-1.2 1.5-2.1 0-1-1-1.8-2.3-1.8-.7 0-1.3.2-1.8.5" />
                      </svg>
                    </span>
                    <span className="settings-source-tile__title">Reddit</span>
                  </span>
                  <span className="settings-source-tile__eyebrow">Community signal</span>
                </div>
                <input
                  type="checkbox"
                  checked={settings.capture.sources.reddit}
                  onChange={(event) =>
                    onChange({
                      ...settings,
                      capture: {
                        ...settings.capture,
                        sources: {
                          ...settings.capture.sources,
                          reddit: event.target.checked
                        }
                      }
                    })
                  }
                />
              </div>
              <p className="settings-source-tile__copy">
                Conversation-heavy posts from your signed-in Reddit home feed, tuned separately from the faster social streams.
              </p>
            </section>
          </div>

          {!hasEnabledSources ? (
            <p className="field-help">Pick at least one source before saving.</p>
          ) : (
            <p className="field-help">Per-source page counts are configured inside each schedule below.</p>
          )}
        </section>

        <section className="settings-card">
          <div className="settings-card__header">
            <div>
              <p className="kicker">Schedule</p>
              <h3>Auto-run</h3>
            </div>
            <p className="settings-card__copy">
              SIFT uses this machine&apos;s timezone automatically for scheduling and edition boundaries.
            </p>
          </div>

          <div className="mini-card">
            <strong>Scheduler overview</strong>
            <span>{scheduleSummary.title}</span>
            <span>{scheduleSummary.detail}</span>
          </div>

          <div className="settings-stack">
            {settings.schedule.rules.map((rule, index) => (
              <section key={rule.id} className="settings-source-tile settings-source-tile--enabled">
                <div className="settings-source-tile__top">
                  <div>
                    <span className="settings-source-tile__title">{rule.label || `Schedule ${index + 1}`}</span>
                    <span className="settings-source-tile__eyebrow">
                      {rule.cadence === "daily" ? "One daily pass" : "Recurring daytime pass"}
                    </span>
                  </div>
                  <input
                    type="checkbox"
                    checked={rule.enabled}
                    onChange={(event) =>
                      updateScheduleRule(rule.id, (current) => ({
                        ...current,
                        enabled: event.target.checked
                      }))
                    }
                  />
                </div>

                <div className="settings-schedule-grid">
                  <label className="field">
                    <span>Schedule label</span>
                    <input
                      value={rule.label}
                      onChange={(event) =>
                        updateScheduleRule(rule.id, (current) => ({
                          ...current,
                          label: event.target.value
                        }))
                      }
                    />
                  </label>

                  <label className="field">
                    <span>Cadence</span>
                    <select
                      value={rule.cadence}
                      onChange={(event) =>
                        updateScheduleRule(rule.id, (current) => ({
                          ...current,
                          cadence: event.target.value as ScheduleRule["cadence"]
                        }))
                      }
                    >
                      <option value="daily">Once a day</option>
                      <option value="interval">Every few hours</option>
                    </select>
                  </label>

                  {rule.cadence === "daily" ? (
                    <label className="field">
                      <span>Daily publish time</span>
                      <input
                        type="time"
                        value={rule.timeOfDay}
                        onChange={(event) =>
                          updateScheduleRule(rule.id, (current) => ({
                            ...current,
                            timeOfDay: event.target.value
                          }))
                        }
                      />
                    </label>
                  ) : (
                    <>
                      <label className="field">
                        <span>Run every hours</span>
                        <input
                          type="number"
                          min={1}
                          max={24}
                          value={rule.intervalHours}
                          onChange={(event) =>
                            updateScheduleRule(rule.id, (current) => ({
                              ...current,
                              intervalHours: Math.min(
                                24,
                                Math.max(1, Number.parseInt(event.target.value || "1", 10) || 1)
                              )
                            }))
                          }
                        />
                      </label>

                      <label className="field">
                        <span>Window start</span>
                        <input
                          type="time"
                          value={rule.windowStart}
                          onChange={(event) =>
                            updateScheduleRule(rule.id, (current) => ({
                              ...current,
                              windowStart: event.target.value
                            }))
                          }
                        />
                      </label>

                      <label className="field">
                        <span>Window end</span>
                        <input
                          type="time"
                          value={rule.windowEnd}
                          onChange={(event) =>
                            updateScheduleRule(rule.id, (current) => ({
                              ...current,
                              windowEnd: event.target.value
                            }))
                          }
                        />
                      </label>
                    </>
                  )}
                </div>

                <div className="settings-source-grid">
                  {(["x", "linkedin", "reddit"] as BrowserSource[]).map((source) => (
                    <section key={source} className="settings-source-tile settings-source-tile--enabled">
                      <div className="settings-source-tile__top">
                        <div>
                          <span className="settings-source-tile__title">
                            {source === "x" ? "X" : source === "linkedin" ? "LinkedIn" : "Reddit"}
                          </span>
                          <span className="settings-source-tile__eyebrow">Pages for this schedule</span>
                        </div>
                      </div>
                      <label className="field">
                        <span>
                          {source === "x" ? "X" : source === "linkedin" ? "LinkedIn" : "Reddit"} pages to browse
                        </span>
                        <input
                          type="number"
                          min={1}
                          value={rule.browsePageCount[source]}
                          onChange={(event) =>
                            updateScheduleRule(rule.id, (current) => ({
                              ...current,
                              browsePageCount: {
                                ...current.browsePageCount,
                                [source]: Math.max(
                                  1,
                                  Number.parseInt(event.target.value || String(current.browsePageCount[source]), 10)
                                  || current.browsePageCount[source]
                                )
                              }
                            }))
                          }
                        />
                      </label>
                    </section>
                  ))}
                </div>

                <div className="button-row">
                  <button
                    className="secondary-button"
                    type="button"
                    onClick={() => removeScheduleRule(rule.id)}
                    disabled={settings.schedule.rules.length === 1}
                  >
                    Remove schedule
                  </button>
                </div>
              </section>
            ))}
          </div>

          <div className="button-row">
            <button className="secondary-button" type="button" onClick={addScheduleRule}>
              Add schedule
            </button>
          </div>
        </section>

        <section className="settings-card">
          <div className="settings-card__header">
            <div>
              <p className="kicker">Cleanup</p>
              <h3>Filter rules</h3>
            </div>
            <p className="settings-card__copy">
              Keep the ranking pass focused by stripping the content you already know you do not want in the paper.
            </p>
          </div>

          <div className="settings-toggle-grid">
            <label className="field field--checkbox">
              <input
                type="checkbox"
                checked={settings.cleanup.hideReplies}
                onChange={(event) =>
                  onChange({
                    ...settings,
                    cleanup: {
                      ...settings.cleanup,
                      hideReplies: event.target.checked
                    }
                  })
                }
              />
              <span>Drop replies</span>
            </label>

            <label className="field field--checkbox">
              <input
                type="checkbox"
                checked={settings.cleanup.hideRetweets}
                onChange={(event) =>
                  onChange({
                    ...settings,
                    cleanup: {
                      ...settings.cleanup,
                      hideRetweets: event.target.checked
                    }
                  })
                }
              />
              <span>Drop reposts</span>
            </label>

            <label className="field field--checkbox">
              <input
                type="checkbox"
                checked={settings.cleanup.removeBait}
                onChange={(event) =>
                  onChange({
                    ...settings,
                    cleanup: {
                      ...settings.cleanup,
                      removeBait: event.target.checked
                    }
                  })
                }
              />
              <span>Filter common engagement bait</span>
            </label>
          </div>

          <div className="settings-copy-grid">
            <label className="field">
              <span>Muted keywords</span>
              <textarea
                value={settings.cleanup.mutedKeywords.join("\n")}
                onChange={(event) =>
                  onChange({
                    ...settings,
                    cleanup: {
                      ...settings.cleanup,
                      mutedKeywords: event.target.value
                        .split("\n")
                        .map((value) => value.trim())
                        .filter(Boolean)
                    }
                  })
                }
                placeholder="One phrase per line"
              />
            </label>

            <label className="field">
              <span>Muted authors</span>
              <textarea
                value={settings.cleanup.mutedAuthors.join("\n")}
                onChange={(event) =>
                  onChange({
                    ...settings,
                    cleanup: {
                      ...settings.cleanup,
                      mutedAuthors: event.target.value
                        .split("\n")
                        .map((value) => value.trim())
                        .filter(Boolean)
                    }
                  })
                }
                placeholder="One handle per line"
              />
            </label>
          </div>
        </section>
      </div>
    </section>
  );
}
