//! The page's views. Logic the views need lives in the native-tested modules beside this one.

use leptos::prelude::*;

pub fn start() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}

#[component]
fn App() -> impl IntoView {
    view! {
        <Header/>
        <main class="loading">
            <p role="status">"Loading room…"</p>
        </main>
    }
}

#[component]
fn Header() -> impl IntoView {
    view! {
        <header class="app-header">
            <a class="brand" href="../rooms">
                <img src="mark.png" alt="" width="24" height="28"/>
                "cocat"
            </a>
            <nav class="crumbs" aria-label="Breadcrumb">
                <a href="../rooms">"My rooms"</a>
                <span aria-hidden="true">"/"</span>
            </nav>
        </header>
    }
}
