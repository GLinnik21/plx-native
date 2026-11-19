// PlxNative Tizen step 1: the C# host.
//
// A plain CoreUIApplication, as flutter-tizen's FlutterApplication is: OnCreate calls the native
// library ONCE (on the main thread, where Ecore lives) and the lifecycle callbacks are forwarded.
// The native library (libplx.so, Rust) makes its own window, draws, and listens for keys; this host
// draws nothing and has no UI of its own.
//
// Everything the host learns is also written to <data>/plx-hello.log (the native side appends to
// the same file), because on a retail TV the log is the only instrument: if the .so never loads,
// the [host] lines below are the only evidence of why.

using System;
using System.Runtime.InteropServices;
using Tizen.Applications;

namespace PlxHello
{
    internal static class Native
    {
        // The brief's spelling. The .NET runtime probes libplx.so for it. UNVERIFIED on a device:
        // nuvio's proven spelling is the full "libnuvio.so". If this does not resolve, App.CreateNative
        // logs the exception and retries after preloading lib/libplx.so by absolute path (libplx.so
        // has SONAME libplx.so, so the bare-name probe then matches the loaded object).
        private const string Lib = "plx";

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern int plx_tizen_create([MarshalAs(UnmanagedType.LPStr)] string dataDir);

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void plx_tizen_pause();

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void plx_tizen_resume();

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void plx_tizen_terminate();

        [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void plx_tizen_locale([MarshalAs(UnmanagedType.LPStr)] string loc);
    }

    internal static class Dl
    {
        internal const int RTLD_NOW = 2;
        internal const int RTLD_GLOBAL = 0x100;

        [DllImport("libdl.so.2")]
        internal static extern IntPtr dlopen([MarshalAs(UnmanagedType.LPStr)] string file, int mode);

        [DllImport("libdl.so.2")]
        internal static extern IntPtr dlerror();
    }

    internal class PlxApp : CoreUIApplication
    {
        // Host-side failure codes (native codes are positive; see plx/src/lib.rs).
        private const int HostDllNotFound = -1;
        private const int HostEntryPointNotFound = -2;
        private const int HostOtherException = -3;

        private static string logPath;
        private bool nativeUp;

        // Lines are mirrored to dlog (`sdb dlog PLXHELLO`) and to the shared log file.
        internal static void Log(string msg)
        {
            string line = "[host " + DateTime.Now.ToString("HH:mm:ss.fff") + "] " + msg;
            try { Tizen.Log.Info("PLXHELLO", line); } catch (Exception) { }
            try
            {
                if (logPath != null)
                {
                    System.IO.File.AppendAllText(logPath, line + "\n");
                }
            }
            catch (Exception) { }
        }

        protected override void OnCreate()
        {
            base.OnCreate();

            string dataDir = this.DirectoryInfo.Data;
            logPath = System.IO.Path.Combine(dataDir, "plx-hello.log");
            Log("host started: OnCreate on the main thread; data dir " + dataDir);
            LogEnvironment();

            int rc = CreateNative(dataDir);
            if (rc != 0)
            {
                Log("native create FAILED rc=" + rc + " (native codes: plx/src/lib.rs; host: -1 dll not found, -2 entry point, -3 other). Exiting.");
                Exit();
                return;
            }
            nativeUp = true;
            Log("native create OK; window and render thread are up");
        }

        protected override void OnPause()
        {
            base.OnPause();
            Log("OnPause");
            if (nativeUp) Native.plx_tizen_pause();
        }

        protected override void OnResume()
        {
            base.OnResume();
            Log("OnResume");
            if (nativeUp) Native.plx_tizen_resume();
        }

        protected override void OnTerminate()
        {
            base.OnTerminate();
            Log("OnTerminate");
            if (nativeUp)
            {
                Native.plx_tizen_terminate();
                nativeUp = false;
            }
        }

        protected override void OnLocaleChanged(LocaleChangedEventArgs e)
        {
            base.OnLocaleChanged(e);
            Log("OnLocaleChanged " + e.Locale);
            if (nativeUp) Native.plx_tizen_locale(e.Locale);
        }

        // The one P/Invoke that matters, with every way it can fail on a device recorded.
        private int CreateNative(string dataDir)
        {
            try
            {
                return Native.plx_tizen_create(dataDir);
            }
            catch (DllNotFoundException e)
            {
                Log("DllNotFoundException for \"plx\": " + e.Message);
                if (!PreloadByPath()) return HostDllNotFound;
                try
                {
                    Log("retrying plx_tizen_create after preload");
                    return Native.plx_tizen_create(dataDir);
                }
                catch (DllNotFoundException e2)
                {
                    Log("still DllNotFoundException after preload: " + e2.Message);
                    return HostDllNotFound;
                }
                catch (Exception e2)
                {
                    Log("exception after preload: " + e2);
                    return HostOtherException;
                }
            }
            catch (EntryPointNotFoundException e)
            {
                Log("EntryPointNotFoundException (the .so loaded but lacks an export): " + e.Message);
                return HostEntryPointNotFound;
            }
            catch (BadImageFormatException e)
            {
                Log("BadImageFormatException (wrong architecture?): " + e.Message);
                return HostDllNotFound;
            }
            catch (Exception e)
            {
                Log("exception from plx_tizen_create: " + e);
                return HostOtherException;
            }
        }

        // dlopen lib/libplx.so by absolute path so the loader's own error text (dlerror) lands in
        // the log: that text is what tells "wrong glibc" from "unsigned library refused" from
        // "wrong architecture".
        private bool PreloadByPath()
        {
            string path = LibraryPath();
            IntPtr h = Dl.dlopen(path, Dl.RTLD_NOW | Dl.RTLD_GLOBAL);
            if (h != IntPtr.Zero)
            {
                Log("preload dlopen(" + path + ") ok");
                return true;
            }
            IntPtr err = Dl.dlerror();
            Log("preload dlopen(" + path + ") FAILED: " + (err == IntPtr.Zero ? "(no dlerror)" : Marshal.PtrToStringAnsi(err)));
            return false;
        }

        // <app>/res/ is DirectoryInfo.Resource; the package's lib/ is its sibling.
        private string LibraryPath()
        {
            return System.IO.Path.GetFullPath(System.IO.Path.Combine(this.DirectoryInfo.Resource, "..", "lib", "libplx.so"));
        }

        private void LogEnvironment()
        {
            Log("runtime " + RuntimeInformation.FrameworkDescription
                + " / os " + RuntimeInformation.OSDescription
                + " / OSArchitecture " + RuntimeInformation.OSArchitecture
                + " / ProcessArchitecture " + RuntimeInformation.ProcessArchitecture
                + " / IntPtr.Size " + IntPtr.Size);
            foreach (string key in new[]
            {
                "http://tizen.org/feature/platform.version",
                "http://tizen.org/feature/platform.native.api.version",
                "http://tizen.org/feature/platform.web.api.version",
                "http://tizen.org/system/platform.name",
                "http://tizen.org/system/model_name",
                "http://tizen.org/system/manufacturer",
                "http://tizen.org/system/build.string",
            })
            {
                LogSystemString(key);
            }
            foreach (string key in new[]
            {
                "http://tizen.org/feature/platform.core.cpu.arch.armv7",
                "http://tizen.org/feature/platform.core.cpu.arch.aarch64",
                "http://tizen.org/feature/platform.core.cpu.arch.x86",
                "http://tizen.org/feature/platform.core.cpu.arch.x86_64",
            })
            {
                LogSystemBool(key);
            }
            string lib = LibraryPath();
            try
            {
                var fi = new System.IO.FileInfo(lib);
                Log("native library " + lib + (fi.Exists ? " exists, " + fi.Length + " bytes" : " DOES NOT EXIST"));
            }
            catch (Exception e)
            {
                Log("native library check failed: " + e.Message);
            }
        }

        private static void LogSystemString(string key)
        {
            try
            {
                string v;
                bool ok = Tizen.System.Information.TryGetValue<string>(key, out v);
                Log(key + " = " + (ok ? v : "(unavailable)"));
            }
            catch (Exception e) { Log(key + " -> " + e.GetType().Name + ": " + e.Message); }
        }

        private static void LogSystemBool(string key)
        {
            try
            {
                bool v;
                bool ok = Tizen.System.Information.TryGetValue<bool>(key, out v);
                Log(key + " = " + (ok ? v.ToString() : "(unavailable)"));
            }
            catch (Exception e) { Log(key + " -> " + e.GetType().Name + ": " + e.Message); }
        }

        private static void Main(string[] args)
        {
            // nuvio: the .NET host puts the process in the TV's locale, and a decimal comma breaks
            // printf("%f")-built JSON. .NET's SetEnvironmentVariable need not reach native getenv,
            // so libplx.so also calls setlocale(LC_NUMERIC, "C") itself.
            Environment.SetEnvironmentVariable("LC_NUMERIC", "C");
            AppDomain.CurrentDomain.UnhandledException += (s, e) => Log("Unhandled exception: " + e.ExceptionObject);
            new PlxApp().Run(args);
        }
    }
}
