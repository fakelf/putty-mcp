using System;
using System.Diagnostics;
using System.IO;
using System.IO.Compression;
using System.Reflection;
using System.Security.Cryptography;
using System.Windows.Forms;

[assembly: AssemblyTitle("PuTTY MCP Connection Assistant")]
[assembly: AssemblyProduct("PuTTY MCP")]
[assembly: AssemblyVersion("4.4.0.0")]
[assembly: AssemblyFileVersion("4.4.0.0")]

internal static class Portable
{
    private const string PayloadResource = "PuTTY-MCP.payload.zip";
    private const string ArchivePrefix = "putty-ai-v4.4.0-x64/";
    private static readonly string[] RuntimeFiles = {
        "putty.exe",
        "putty-ai-mcp.exe",
        "scripts/connection-assistant.ps1",
        "scripts/check-mcp.ps1",
        "scripts/generate-client-config.ps1",
    };

    [STAThread]
    private static void Main()
    {
        try
        {
            string localData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
            if (String.IsNullOrEmpty(localData))
                throw new IOException("The current Windows user's local application data folder is unavailable.");
            string runtime = PreparePayload(Path.Combine(localData, "PuTTY-MCP-Portable"));
            string script = Path.Combine(runtime, "scripts", "connection-assistant.ps1");
            string powershell = Path.Combine(Environment.SystemDirectory, "WindowsPowerShell", "v1.0", "powershell.exe");
            var start = new ProcessStartInfo {
                FileName = powershell,
                Arguments = "-NoProfile -STA -WindowStyle Hidden -ExecutionPolicy Bypass -File \"" + script + "\"",
                WorkingDirectory = runtime,
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            using (Process.Start(start)) { }
        }
        catch (Exception error)
        {
            MessageBox.Show(error.Message, "PuTTY MCP", MessageBoxButtons.OK, MessageBoxIcon.Error);
        }
    }

    private static string PreparePayload(string cacheRoot)
    {
        using (Stream payload = typeof(Portable).Assembly.GetManifestResourceStream(PayloadResource))
        {
            if (payload == null)
                throw new InvalidDataException("Embedded PuTTY MCP program files are missing.");
            string fingerprint = Digest(payload);
            payload.Position = 0;
            cacheRoot = Path.GetFullPath(cacheRoot);
            string runtime = Path.Combine(cacheRoot, "4.4.0-" + fingerprint.Substring(0, 16));
            using (var zip = new ZipArchive(payload, ZipArchiveMode.Read))
            {
                if (!Directory.Exists(runtime))
                    Extract(zip, cacheRoot, runtime);
                CheckRuntimeFiles(zip, runtime);
            }
            return runtime;
        }
    }

    private static void Extract(ZipArchive zip, string cacheRoot, string runtime)
    {
        Directory.CreateDirectory(cacheRoot);
        string staging = Path.Combine(cacheRoot, "unpacking-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(staging);
        string boundary = staging + Path.DirectorySeparatorChar;
        try
        {
            foreach (ZipArchiveEntry entry in zip.Entries)
            {
                string name = entry.FullName.Replace('\\', '/');
                if (!name.StartsWith(ArchivePrefix, StringComparison.Ordinal))
                    throw new InvalidDataException("Unexpected embedded PuTTY MCP file: " + name);
                if (name.EndsWith("/", StringComparison.Ordinal))
                    continue;
                string relative = name.Substring(ArchivePrefix.Length);
                string destination = Path.GetFullPath(Path.Combine(staging, relative));
                if (!destination.StartsWith(boundary, StringComparison.OrdinalIgnoreCase))
                    throw new InvalidDataException("Invalid embedded PuTTY MCP file path.");
                Directory.CreateDirectory(Path.GetDirectoryName(destination));
                using (Stream source = entry.Open())
                using (var file = new FileStream(destination, FileMode.CreateNew, FileAccess.Write))
                    source.CopyTo(file);
            }
            CheckRuntimeFiles(zip, staging);
            try
            {
                Directory.Move(staging, runtime);
            }
            catch (IOException)
            {
                // Another launch may have finished extracting the same archive.
                if (!Directory.Exists(runtime))
                    throw;
            }
        }
        finally
        {
            if (Directory.Exists(staging))
                Directory.Delete(staging, true);
        }
    }

    private static void CheckRuntimeFiles(ZipArchive zip, string runtime)
    {
        foreach (string relative in RuntimeFiles)
        {
            string entryName = ArchivePrefix + relative;
            ZipArchiveEntry entry = zip.GetEntry(entryName) ?? zip.GetEntry(entryName.Replace('/', '\\'));
            string file = Path.Combine(runtime, relative);
            if (entry == null || !File.Exists(file))
                throw new IOException("A PuTTY MCP runtime file is missing: " + file);
            using (Stream embedded = entry.Open())
            using (Stream extracted = File.OpenRead(file))
            {
                if (Digest(embedded) != Digest(extracted))
                    throw new InvalidDataException("A PuTTY MCP runtime file is damaged: " + file);
            }
        }
    }

    private static string Digest(Stream data)
    {
        using (SHA256 sha256 = SHA256.Create())
            return BitConverter.ToString(sha256.ComputeHash(data)).Replace("-", "").ToLowerInvariant();
    }
}
