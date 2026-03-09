namespace VaultNote.Mobile;

public partial class AppShell : Shell
{
	public AppShell()
	{
		InitializeComponent();

		Routing.RegisterRoute(nameof(CreateNotePage), typeof(CreateNotePage));
		Routing.RegisterRoute(nameof(ListNotesPage), typeof(ListNotesPage));
	}
}
