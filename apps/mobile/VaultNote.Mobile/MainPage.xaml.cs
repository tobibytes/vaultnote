using Grpc.Core;
using Grpc.Net.Client;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class MainPage : ContentPage
{
	private readonly VaultNoteService.VaultNoteServiceClient _client;
	private bool _pingInFlight;

	public MainPage()
	{
		InitializeComponent();

		AppContext.SetSwitch("System.Net.Http.SocketsHttpHandler.Http2UnencryptedSupport", true);
		var channel = GrpcChannel.ForAddress("http://127.0.0.1:50051");
		_client = new VaultNoteService.VaultNoteServiceClient(channel);
	}

	private async void OnCreateNoteClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new CreateNotePage());
	}

	private async void OnListNotesClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new ListNotesPage());
	}

	private async void OnPingClicked(object? sender, EventArgs e)
	{
		if (_pingInFlight)
		{
			return;
		}

		try
		{
			_pingInFlight = true;
			PingButton.IsEnabled = false;
			PingStatusLabel.Text = "Pinging backend...";

			var response = await _client.PingAsync(new PingRequest());
			PingStatusLabel.Text = $"Ping OK: {response.Message}";
		}
		catch (RpcException rpcEx)
		{
			PingStatusLabel.Text = $"gRPC error: {rpcEx.Status.StatusCode} ({rpcEx.Status.Detail})";
		}
		catch (Exception ex)
		{
			PingStatusLabel.Text = $"Unexpected error: {ex.Message}";
		}
		finally
		{
			PingButton.IsEnabled = true;
			_pingInFlight = false;
		}
	}
}
