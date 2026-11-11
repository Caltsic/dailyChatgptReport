using DailyReport.Core;

namespace DailyReport.Server;

public record DepartmentSettings(string Id, string Name, string TimeZone, int Port, string AdvertisedUrl, string ApiBaseUrl, string Model, int ChunkCharacters = 18000);
public record Member(string Id, string Name, bool Active = true);
public record Device(string Id, string MemberId, string Name, string TokenHash, bool Revoked = false);
public record InviteRecord(string Id, string MemberId, DateTimeOffset ExpiresAt, string? PairingRequestId = null, string? DeviceId = null, string? ProtectedToken = null);
public record Snapshot(string Id, string MemberId, string Day, int Version, string RequestId, string Hash, string FileName, DateTimeOffset ReceivedAt, DateTimeOffset CapturedAt, int SessionCount, List<string> Warnings);
public record DayState(string Id, string MemberId, string Day, bool Closed = false);
public record Notice(string Id, string MemberId, string Day, string Reason, DateTimeOffset CreatedAt, bool Fulfilled = false);
public record Operation(string Id, string MemberId, string Hash, string ResultId, string Kind);
public record Job(string Id, string Kind, string Day, string MemberId, string SnapshotId, string State, DateTimeOffset CreatedAt, string? Error = null, JobProgress? Progress = null);
public record Audit(string Id, DateTimeOffset At, string Action, string Subject);
public record DashboardRow(string MemberId, string Name, string SubmissionStatus, string ReportStatus, string CapturedAt, int SnapshotVersion, int ReportVersion, string FeedbackStatus, string? ReportId, string AnalysisProgress = "", double StagePercent = 0, string? JobId = null);
public sealed class DomainException(string message, int status = 409) : Exception(message) { public int Status { get; } = status; }
