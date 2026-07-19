// ===== File: HostCalls.cs — buffer plumbing for the (ptr,len,out,cap,out_len) ABI =====
// Unifies both host conventions:
//  - legacy: rc < 0 error, rc == 0 ok, rc > out_cap = required size (retry);
//  - v1:     rc == 0 ok, rc == 6 (OutputBufferTooSmall) with the required
//            size written to out_len (retry), other positive rc = AbiError.
// The two never collide: a legacy "required size" is always > out_cap
// (≥ 64 KiB here), far above the 1..24 AbiError range.

#nullable enable

using System;

namespace TentaFlow.Sdk;

internal static class HostCalls
{
    internal delegate int InOutFn(int inPtr, int inLen, int outPtr, int outCap, int outLenPtr);

    internal const int DefaultOutCap = 64 * 1024;
    internal const int MaxOutCap = 8 * 1024 * 1024;

    internal readonly struct CallResult
    {
        public readonly int Code;
        public readonly byte[] Data;

        public CallResult(int code, byte[] data)
        {
            Code = code;
            Data = data;
        }
    }

    internal static unsafe CallResult CallInOut(
        InOutFn fn, ReadOnlySpan<byte> input,
        int initialCap = DefaultOutCap, int maxCap = MaxOutCap)
    {
        var buf = new byte[Math.Max(initialCap, 16)];
        for (int attempt = 0; ; attempt++)
        {
            int outLen = 0;
            int rc;
            fixed (byte* inP = input)
            fixed (byte* outP = buf)
            {
                rc = fn(
                    input.IsEmpty ? 0 : (int)inP, input.Length,
                    (int)outP, buf.Length, (int)(&outLen));
            }
            if (rc == 0)
            {
                if (outLen < 0 || outLen > buf.Length)
                {
                    return new CallResult((int)AbiError.Operation, System.Array.Empty<byte>());
                }
                var data = new byte[outLen];
                System.Array.Copy(buf, data, outLen);
                return new CallResult(0, data);
            }
            if (attempt == 0)
            {
                // v1 retry: OutputBufferTooSmall + required size in out_len.
                if (rc == (int)AbiError.OutputBufferTooSmall
                    && outLen > buf.Length && outLen <= maxCap)
                {
                    buf = new byte[outLen];
                    continue;
                }
                // Legacy retry: rc itself is the required size.
                if (rc > buf.Length && rc <= maxCap)
                {
                    buf = new byte[rc];
                    continue;
                }
            }
            return new CallResult(rc, System.Array.Empty<byte>());
        }
    }

    /// <summary>UTF-8 encode helper shared by string-based wrappers.</summary>
    internal static byte[] Utf8(string s) => System.Text.Encoding.UTF8.GetBytes(s);

    internal static string Utf8(byte[] bytes) => System.Text.Encoding.UTF8.GetString(bytes);
}
