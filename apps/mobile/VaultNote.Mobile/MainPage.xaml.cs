using Grpc.Core;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class MainPage : ContentPage
{
	private bool _pingInFlight;

	public MainPage()
	{
		InitializeComponent();
	}

	private async void OnCreateNoteClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new CreateNotePage());
	}

	private async void OnListNotesClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new ListNotesPage());
	}

	private async void OnSearchNotesClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new SearchNotesPage());
	}

	private async void OnAskVaultClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new AskVaultPage());
	}

	private async void OnLoginClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new LoginPage());
	}

	private async void OnRegisterClicked(object? sender, EventArgs e)
	{
		await Navigation.PushAsync(new RegisterPage());
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

			var response = await GrpcClientService.Client.PingAsync(new PingRequest());
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
