export interface AppInfo { role: 'Member' | 'Leader'; version: string; day: string; root: string }
export interface Invitation { url: string; fingerprint: string; code: string; department: string; modelDestination: string; timeZone: string; protocol: number }
export interface Pairing { url: string; fingerprint: string; identity: { memberName: string; departmentName: string; modelDestination: string; timeZone: string } }
export interface Outbox { id: string; createdAt: string; kind: string; error?: string }
export interface SourceStatus { tool: string; label: string; enabled: boolean; path: string; effectivePath: string; detected: boolean }
export interface MemberInit { pairing?: Pairing; codexRoot: string; sources: SourceStatus[]; queue: Outbox[] }
export interface Finding { text: string; status?: string; evidenceIds?: string[]; reportIds?: string[]; personalStatement?: boolean }
export interface Report {
  id: string; memberId?: string; memberName?: string; department: string; day: string; version: number;
  snapshotVersion?: number; capturedAt?: string; sessionCount?: number; timeZone?: string; createdAt: string;
  finalizedAt?: string; releasedAt?: string; body?: string; sources: Record<string, string>;
  warnings?: string[]; missingMembers?: string[];
  content: Record<string, Finding[]>;
  feedback?: { text: string; noSupplement: boolean; author: string; submittedAt: string };
}
export interface MemberStatus {
  day: string; submissionStatus: string; reportStatus: string; report?: Report; receipt?: { version: number; receivedAt: string };
  notices: { id: string; day: string; reason: string; fulfilled: boolean }[];
  departmentName: string; modelDestination: string; timeZone: string;
  capturedAt?: string; sessionCount: number; captureWarnings?: string[]; analysisProgress?: string;
}
export interface MemberState { status?: MemberStatus; queue: Outbox[]; syncMessage: string; draft: string; online?: boolean; draftFromPrevious?: boolean; feedbackBlockedReason?: string }
export interface CaptureSession {
  id: string; title: string; project: string; firstAt: string; lastAt: string;
  eventCount: number; warnings: string[]; selected: boolean; isNew: boolean;
  tool?: string; toolLabel?: string;
}
export interface CaptureDraft {
  draftId: string; day: string; timeZone: string; capturedAt: string;
  sessions: CaptureSession[]; warnings: string[];
}
export interface CaptureEvent { id: string; sessionId: string; timestamp: string; kind: string; text: string; source: string; tool?: string }
export interface CapturePreview { events: CaptureEvent[]; warnings: string[] }
export interface SubmissionResult { received?: boolean; syncMessage?: string; sessionCount?: number; eventCount?: number }
export interface Settings { id: string; name: string; timeZone: string; port: number; advertisedUrl: string; addressMode?: 'auto' | 'manual'; apiBaseUrl: string; model: string; chunkCharacters: number; contextTokens?: number; outputReserveTokens?: number; chunkTokens?: number }
export interface DashboardRow { memberId: string; name: string; submissionStatus: string; reportStatus: string; capturedAt: string; snapshotVersion: number; reportVersion: number; feedbackStatus: string; reportId?: string; analysisProgress: string; stagePercent: number; jobId?: string }
export interface Job {
  id: string; kind: string; day: string; memberId: string; state: string; createdAt: string; error?: string; reportId?: string; finishedAt?: string;
  progress?: { stage: string; completed: number; total: number; current: string; activity: string; startedAt: string; updatedAt: string; requestStartedAt?: string; requests: number; retries: number; repairs: number; active?: number; failed?: number; estimatedTokens?: number; pieces?: { id: string; label: string; state: string; error?: string }[]; events: { at: string; message: string }[] };
}
export interface Dashboard { rows: DashboardRow[]; jobs: Job[]; summary?: Report; summaryStale: boolean }
