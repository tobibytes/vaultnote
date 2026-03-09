using Grpc.Core;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class ListNotesPage : ContentPage
{
    private bool _loadInFlight;

    public ListNotesPage()
    {
        InitializeComponent();
    }

    protected override async void OnAppearing()
    {
        base.OnAppearing();
        await LoadNotesAsync();
    }

    private async void OnRefreshClicked(object? sender, EventArgs e)
    {
        await LoadNotesAsync();
    }

    private async Task LoadNotesAsync()
    {
        if (_loadInFlight)
        {
            return;
        }

        try
        {
            _loadInFlight = true;
            RefreshButton.IsEnabled = false;
            StatusLabel.Text = "Loading…";

            var response = await GrpcClientService.Client.ListNotesAsync(new ListNotesRequest());
            var items = response.Notes
                .Select(n => new NoteItem { Title = n.Title, Content = n.Content })
                .ToList();

            NotesCollection.ItemsSource = items;
            StatusLabel.Text = $"{items.Count} note(s)";
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
            RefreshButton.IsEnabled = true;
            _loadInFlight = false;
        }
    }
}

