namespace VaultNote.Mobile;

/// <summary>Lightweight view-model used to data-bind note data in collection views.</summary>
public sealed class NoteItem
{
    public string Title { get; init; } = string.Empty;
    public string Content { get; init; } = string.Empty;
}
