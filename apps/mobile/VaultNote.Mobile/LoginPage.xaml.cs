using Grpc.Core;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class LoginPage : ContentPage
{
    private bool _inFlight;

    public LoginPage()
    {
        InitializeComponent();
    }

    private async void OnLoginClicked(object? sender, EventArgs e)
    {
        if (_inFlight)
        {
            return;
        }

        var email = EmailEntry.Text?.Trim() ?? string.Empty;
        var password = PasswordEntry.Text ?? string.Empty;

        if (string.IsNullOrEmpty(email))
        {
            StatusLabel.Text = "Email is required.";
            return;
        }
        if (string.IsNullOrEmpty(password))
        {
            StatusLabel.Text = "Password is required.";
            return;
        }

        try
        {
            _inFlight = true;
            LoginButton.IsEnabled = false;
            StatusLabel.Text = "Logging in...";

            var response = await GrpcClientService.Client.LoginAsync(new LoginRequest
            {
                Email = email,
                Password = password,
            });

            StatusLabel.Text = $"Login successful! User ID: {response.UserId[..Math.Min(8, response.UserId.Length)]}…";
            // TODO: store the JWT token for authenticated requests
        }
        catch (RpcException rpcEx)
        {
            StatusLabel.Text = rpcEx.Status.StatusCode == StatusCode.Unauthenticated
                ? "Invalid email or password."
                : $"gRPC error: {rpcEx.Status.StatusCode} ({rpcEx.Status.Detail})";
        }
        catch (Exception ex)
        {
            StatusLabel.Text = $"Error: {ex.Message}";
        }
        finally
        {
            LoginButton.IsEnabled = true;
            _inFlight = false;
        }
    }
}
