using Grpc.Core;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class RegisterPage : ContentPage
{
    private bool _inFlight;

    public RegisterPage()
    {
        InitializeComponent();
    }

    private async void OnRegisterClicked(object? sender, EventArgs e)
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
        if (password.Length < 8)
        {
            StatusLabel.Text = "Password must be at least 8 characters.";
            return;
        }

        try
        {
            _inFlight = true;
            RegisterButton.IsEnabled = false;
            StatusLabel.Text = "Creating account...";

            var response = await GrpcClientService.Client.RegisterAsync(new RegisterRequest
            {
                Email = email,
                Password = password,
            });

            StatusLabel.Text = $"Account created! User ID: {response.UserId[..Math.Min(8, response.UserId.Length)]}…";
            EmailEntry.Text = string.Empty;
            PasswordEntry.Text = string.Empty;
        }
        catch (RpcException rpcEx)
        {
            StatusLabel.Text = rpcEx.Status.StatusCode == StatusCode.AlreadyExists
                ? "An account with that email already exists."
                : $"gRPC error: {rpcEx.Status.StatusCode} ({rpcEx.Status.Detail})";
        }
        catch (Exception ex)
        {
            StatusLabel.Text = $"Error: {ex.Message}";
        }
        finally
        {
            RegisterButton.IsEnabled = true;
            _inFlight = false;
        }
    }
}
