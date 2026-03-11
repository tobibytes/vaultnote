using Grpc.Core;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class SearchNotesPage : ContentPage
{
    private bool _searchInFlight;

    public SearchNotesPage()
    {
        InitializeComponent();
    }

    private async void OnSearchClicked(object? sender, EventArgs e)
    {
        await RunSearchAsync();
    }

    private async Task RunSearchAsync()
    {
        if (_searchInFlight)
        {
            return;
        }

        var query = QueryEntry.Text?.Trim() ?? string.Empty;
        if (string.IsNullOrEmpty(query))
        {
            StatusLabel.Text = "Please enter a search term.";
            return;
        }

        var results = new List<NoteItem>();

        try
        {
            _searchInFlight = true;
            SearchButton.IsEnabled = false;
            StatusLabel.Text = "Searching…";
            ResultsCollection.ItemsSource = null;

            using var streamingCall = GrpcClientService.Client.SearchNotes(
                new SearchNotesRequest { Query = query });

            await foreach (var note in streamingCall.ResponseStream.ReadAllAsync())
            {
                results.Add(new NoteItem { Title = note.Title, Content = note.Content });
            }

            ResultsCollection.ItemsSource = results;
            StatusLabel.Text = results.Count == 0
                ? "No notes matched your query."
                : $"{results.Count} result(s) for \"{query}\"";
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
            SearchButton.IsEnabled = true;
            _searchInFlight = false;
        }
    }
}

