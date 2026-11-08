using System.Runtime.InteropServices;

namespace LiveListen.Interop;

/// <summary>C ABI of the Rust core (crates/live-listen-ffi).</summary>
internal static unsafe partial class Native
{
    private const string Lib = "livelisten_native";

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint ll_start(string? configDir, delegate* unmanaged[Cdecl]<nint, nint, void> callback, nint user);

    [LibraryImport(Lib, StringMarshalling = StringMarshalling.Utf8)]
    internal static partial nint ll_call(nint handle, string request);

    [LibraryImport(Lib)]
    internal static partial void ll_free(nint text);

    [LibraryImport(Lib)]
    internal static partial void ll_destroy(nint handle);

    [LibraryImport(Lib)]
    internal static partial nint ll_version();
}
