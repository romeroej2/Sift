export type SyncStatus = "idle" | "running" | "success" | "error";
export type BrowserSource = "x" | "linkedin";
export type EditionView = "consolidated" | "x" | "linkedin";
export type SyncProgressStage =
  | "starting"
  | "navigating-home"
  | "capturing-feed"
  | "ranking-items"
  | "building-edition"
  | "saving-edition"
  | "complete"
  | "error";

export interface CleanupSettings {
  hideReplies: boolean;
  hideRetweets: boolean;
  removeBait: boolean;
  mutedKeywords: string[];
  mutedAuthors: string[];
}

export interface LmStudioSettings {
  baseUrl: string;
  authToken: string | null;
  selectedModel: string | null;
  includeImages: boolean;
}

export interface CaptureSourcesSettings {
  x: boolean;
  linkedin: boolean;
}

export interface CaptureSettings {
  sources: CaptureSourcesSettings;
  browsePageCount: Record<BrowserSource, number>;
}

export interface ScheduleSettings {
  enabled: boolean;
  timeOfDay: string;
  timezone: string;
}

export interface XConnectionSummary {
  userId: string;
  handle: string;
  name: string;
  connectedAt: string;
}

export interface BrowserSessionState {
  isOpen: boolean;
  isVisible: boolean;
  isAuthenticated: boolean;
  lastKnownUrl: string | null;
  mode: string;
}

export interface UserSettings {
  schedule: ScheduleSettings;
  cleanup: CleanupSettings;
  lmStudio: LmStudioSettings;
  capture: CaptureSettings;
}

export interface EditionCard {
  itemId: string;
  authorName: string;
  authorHandle: string;
  sourceUrl: string;
  postedAt: string;
  category: string;
  headline: string;
  summary: string;
  whyItMatters: string;
  leadImage?: EditionImage;
}

export interface EditionImage {
  path: string;
  sourceUrl: string;
  mimeType: string;
  alt: string;
}

export interface EditionSection {
  id: string;
  title: string;
  dek: string;
  cards: EditionCard[];
}

export interface Edition {
  id: string;
  editionDate: string;
  title: string;
  frontPageSummary: string;
  createdAt: string;
  view: EditionView;
  sections: EditionSection[];
}

export interface SyncRun {
  id: string;
  startedAt: string;
  finishedAt: string | null;
  status: SyncStatus;
  itemCount: number;
  keptCount: number;
  errorMessage: string | null;
  editionId: string | null;
}

export interface SyncProgressEvent {
  runId: string;
  reason: "manual" | "scheduled";
  status: Exclude<SyncStatus, "idle">;
  stage: SyncProgressStage;
  message: string;
  itemCount: number | null;
  newItemCount: number | null;
  keptCount: number | null;
  editionId: string | null;
  timestamp: string;
}

export interface ModelDescriptor {
  id: string;
  displayName: string;
  loaded: boolean;
}

export interface BootstrapState {
  settings: UserSettings;
  editions: Edition[];
  latestRun: SyncRun | null;
  xConnection: XConnectionSummary | null;
}

export interface LmStudioHealth {
  ok: boolean;
  serverLabel: string;
  models: ModelDescriptor[];
  message: string;
}
