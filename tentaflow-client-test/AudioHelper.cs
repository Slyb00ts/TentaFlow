using System.Diagnostics;
using System.Runtime.InteropServices;

namespace TentaFlow.Client.Test;

/// <summary>
/// Cross-platform audio playback helper.
/// - Linux: paplay/aplay przez stdin (bez pliku tymczasowego)
/// - macOS: afplay (wymaga pliku)
/// - Windows: PowerShell z System.Media.SoundPlayer (wymaga pliku)
/// </summary>
public static class AudioHelper
{
    public static void PlayAudio(byte[] audioData)
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Linux))
        {
            // Linux: graj przez stdin - bez pliku tymczasowego!
            var player = File.Exists("/usr/bin/paplay") ? "paplay" : "aplay";
            var psi = new ProcessStartInfo
            {
                FileName = player,
                Arguments = player == "aplay" ? "-q -" : "",  // aplay wymaga "-", paplay nie
                UseShellExecute = false,
                RedirectStandardInput = true,
                RedirectStandardError = true
            };

            using var process = Process.Start(psi);
            if (process != null)
            {
                process.StandardInput.BaseStream.Write(audioData, 0, audioData.Length);
                process.StandardInput.Close();
                process.WaitForExit();
            }
        }
        else
        {
            // macOS i Windows wymagają pliku
            var tempFile = Path.Combine(Path.GetTempPath(), $"tentaflow_audio_{Guid.NewGuid()}.wav");
            File.WriteAllBytes(tempFile, audioData);

            try
            {
                ProcessStartInfo psi;

                if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
                {
                    // Windows: PowerShell z System.Media.SoundPlayer
                    psi = new ProcessStartInfo
                    {
                        FileName = "powershell",
                        Arguments = $"-c \"(New-Object System.Media.SoundPlayer '{tempFile}').PlaySync()\"",
                        UseShellExecute = false,
                        RedirectStandardError = true,
                        CreateNoWindow = true
                    };
                }
                else
                {
                    // macOS: afplay
                    psi = new ProcessStartInfo
                    {
                        FileName = "afplay",
                        Arguments = $"\"{tempFile}\"",
                        UseShellExecute = false,
                        RedirectStandardError = true
                    };
                }

                using var process = Process.Start(psi);
                process?.WaitForExit();
            }
            finally
            {
                if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
                    Thread.Sleep(100);

                try { if (File.Exists(tempFile)) File.Delete(tempFile); }
                catch { /* ignore cleanup errors */ }
            }
        }
    }
}
