using Grpc.Core;
using VaultNote.Proto;

namespace VaultNote.Mobile;

public partial class AskVaultPage : ContentPage
{
    private bool _inFlight;

    public AskVaultPage()
    {
        InitializeComponent();
    }

    private async void OnAskClicked(object? sender, EventArgs e)
    {
        if (_inFlight)
        {
            return;
        }

        var question = QuestionEntry.Text?.Trim() ?? string.Empty;
        if (string.IsNullOrEmpty(question))
        {
            StatusLabel.Text = "Please enter a question.";
            return;
        }

        try
        {
            _inFlight = true;
            AskButton.IsEnabled = false;
            StatusLabel.Text = "Waiting for answer…";
            AnswerLabel.Text = string.Empty;

            // AskVault is bidirectional streaming: send one question, receive
            // streamed tokens that we concatenate into a full answer.
            using var call = GrpcClientService.Client.AskVault();

            await call.RequestStream.WriteAsync(new AskVaultRequest { Question = question });
            await call.RequestStream.CompleteAsync();

            var answerBuilder = new System.Text.StringBuilder();

            await foreach (var response in call.ResponseStream.ReadAllAsync())
            {
                answerBuilder.Append(response.Token);
                AnswerLabel.Text = answerBuilder.ToString();
                // Scroll to bottom so the user can see tokens as they arrive.
                await AnswerScroll.ScrollToAsync(0, AnswerLabel.Height, animated: false);
            }

            StatusLabel.Text = "Done.";
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
            AskButton.IsEnabled = true;
            _inFlight = false;
        }
    }
}
