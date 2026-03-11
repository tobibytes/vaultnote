using Grpc.Core;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class CreateNotePage : ContentPage
{
    private bool _saveInFlight;

    public CreateNotePage()
    {
        InitializeComponent();
    }

    private async void OnSaveClicked(object? sender, EventArgs e)
    {
        if (_saveInFlight)
        {
            return;
        }

        var title = TitleEntry.Text?.Trim() ?? string.Empty;
        if (string.IsNullOrEmpty(title))
        {
            StatusLabel.Text = "Title is required.";
            return;
        }

        var content = ContentEditor.Text ?? string.Empty;

        try
        {
            _saveInFlight = true;
            SaveButton.IsEnabled = false;
            StatusLabel.Text = "Saving...";

            var response = await GrpcClientService.Client.CreateNoteAsync(new CreateNoteRequest
            {
                Title = title,
                Content = content,
            });

            var note = response.Note;
            var shortId = note?.Id is { Length: >= 8 } id ? id[..8] : note?.Id ?? string.Empty;
            StatusLabel.Text = $"Saved: {note?.Title} (id: {shortId}…)";
            TitleEntry.Text = string.Empty;
            ContentEditor.Text = string.Empty;
        }
        catch (RpcException rpcEx)
        {
            StatusLabel.Text = $"gRPC error: {rpcEx.Status.StatusCode} ({rpcEx.Status.Detail})";
        }
        catch (Exception ex)
        {
            StatusLabel.Text = $"Error: {ex.Message}";
        }
        finally
        {
            SaveButton.IsEnabled = true;
            _saveInFlight = false;
        }
    }
}
