// ===== File: Abi.cs — host-function ABI error codes and exception type =====
// Two conventions coexist on the host:
//  - legacy host functions (storage, http, llm, ui, events, secrets, log)
//    return negative codes from host_functions/mod.rs (ABI_ERR_*);
//  - F1a "v1" host functions (sql, state, flow, vector, ...) return the
//    positive AbiError codes from core/errors.rs (0 = Ok).
// Both sets are mirrored here; wrappers know which family they call.

#nullable enable

using System;

namespace TentaFlow.Sdk;

/// <summary>Legacy negative ABI codes (host_functions/mod.rs).</summary>
public static class LegacyAbi
{
    public const int Ok = 0;
    public const int ErrPermission = -1;
    public const int ErrOperation = -2;
    public const int ErrTimeout = -3;
    public const int ErrRateLimit = -4;
    public const int ErrNotFound = -5;
    public const int ErrBufferTooSmall = -6;
}

/// <summary>Canonical F1a ABI error codes (core/errors.rs; 0 = success).</summary>
public enum AbiError
{
    Ok = 0,
    Permission = 1,
    NotFound = 2,
    NoAvailableTarget = 3,
    Timeout = 4,
    Operation = 5,
    OutputBufferTooSmall = 6,
    Conflict = 7,
    SqlSyntax = 8,
    SqlConstraint = 9,
    SqlNoResult = 10,
    QuotaExceeded = 11,
    CameraUnreachable = 12,
    CameraAuthFailed = 13,
    CameraVendorUnsupported = 14,
    StreamNotFound = 15,
    StreamClosed = 16,
    Backpressure = 17,
    RecordingNotFound = 18,
    RecordingPurged = 19,
    RecordingTimeOutOfRing = 20,
    PayloadTooLarge = 21,
    GateNotSatisfied = 22,
    FrameTokenInvalid = 23,
    FramePurged = 24,
}

public static class AbiErrorExtensions
{
    /// <summary>
    /// Decodes a raw i32 from a v1 host function. Unknown codes collapse to
    /// Operation so version skew never surfaces a phantom variant.
    /// </summary>
    public static AbiError FromCode(int code) =>
        code is >= 0 and <= 24 ? (AbiError)code : AbiError.Operation;
}

/// <summary>Raised by SDK wrappers when a host function reports an error.</summary>
public sealed class HostCallException : Exception
{
    /// <summary>Raw i32 returned by the host function.</summary>
    public int Code { get; }

    public HostCallException(string function, int code)
        : base($"host function {function} failed with code {code}")
    {
        Code = code;
    }
}
