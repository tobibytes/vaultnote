namespace VaultNote.Mobile;

public partial class AppShell : Shell
{
	public AppShell()
	{
		InitializeComponent();

		Routing.RegisterRoute(nameof(CreateNotePage), typeof(CreateNotePage));
		Routing.RegisterRoute(nameof(ListNotesPage), typeof(ListNotesPage));
		Routing.RegisterRoute(nameof(SearchNotesPage), typeof(SearchNotesPage));
		Routing.RegisterRoute(nameof(AskVaultPage), typeof(AskVaultPage));
		Routing.RegisterRoute(nameof(LoginPage), typeof(LoginPage));
		Routing.RegisterRoute(nameof(RegisterPage), typeof(RegisterPage));
	}
}
