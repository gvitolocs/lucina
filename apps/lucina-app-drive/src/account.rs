//! The one place this app touches the account: the system account service owns the token.

use std::sync::Arc;

use lucina_account::system;
use lucina_account::ui::SignInView;

/// The sign-in screen. `done` fires when the machine is signed in, from any screen.
pub fn sign_in(changed: Arc<dyn Fn() + Send + Sync>, done: Arc<dyn Fn() + Send + Sync>) -> Option<SignInView> {
    Some(SignInView::start(changed, done))
}

/// The server said 401: the service asks it again, and signs every app out if the token is dead.
pub fn rejected() {
    system::report_rejected();
}
