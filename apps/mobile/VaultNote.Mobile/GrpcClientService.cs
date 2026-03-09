using Grpc.Net.Client;
using VaultNote.Proto;

namespace VaultNote.Mobile;

/// <summary>
/// Provides a shared, lazily-initialised gRPC channel and client so that
/// every page talks to the same backend connection.
/// </summary>
public static class GrpcClientService
{
    private static readonly Lazy<VaultNoteService.VaultNoteServiceClient> _client = new(() =>
    {
        AppContext.SetSwitch("System.Net.Http.SocketsHttpHandler.Http2UnencryptedSupport", true);
        var channel = GrpcChannel.ForAddress(BackendAddress!);
        return new VaultNoteService.VaultNoteServiceClient(channel);
    });

    /// <summary>
    /// Base address of the backend gRPC server.
    /// Override at app start-up before first access if needed (e.g., for
    /// device-specific addresses such as Android emulator 10.0.2.2).
    /// </summary>
    public static string BackendAddress { get; set; } = "http://127.0.0.1:50051";

    /// <summary>The shared gRPC client instance.</summary>
    public static VaultNoteService.VaultNoteServiceClient Client => _client.Value;
}
