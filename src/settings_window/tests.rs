use super::*;

#[test]
fn tab_tags_keep_legacy_aliases() {
    assert_eq!(Tab::from_tag("floating-panel"), Tab::FloatingPanel);
    assert_eq!(Tab::from_tag("popup"), Tab::Popup);
    assert_eq!(Tab::from_tag("customize"), Tab::Popup);
    assert_eq!(Tab::from_tag("schedule"), Tab::Schedule);
    assert_eq!(Tab::from_tag("limit-activation"), Tab::Schedule);
    assert_eq!(Tab::Appearance.tag(), "appearance");
    assert_eq!(Tab::from_tag("integrations"), Tab::Integrations);
}

#[test]
fn floating_panel_has_an_independent_settings_page_in_both_icon_modes() {
    for colored in [false, true] {
        let items = navigation::root_nav_items("#123456", colored);
        let item = items
            .iter()
            .find(|item| item.tag.as_deref() == Some("floating-panel"))
            .unwrap();
        assert_eq!(item.content, "Floating panel");
        assert_eq!(
            item.icon_path.as_ref().unwrap().0,
            crate::icons::data("desktop")
        );
    }
    assert_eq!(
        RenderedPage::Root(Tab::FloatingPanel).page_key(),
        "settings-page-floating-panel"
    );
}

#[test]
fn root_navigation_keeps_customize_separate_from_providers() {
    let items = navigation::root_nav_items("#123456", false);
    assert!(
        items
            .iter()
            .any(|item| item.tag.as_deref() == Some("providers"))
    );
    assert!(
        items
            .iter()
            .any(|item| item.tag.as_deref() == Some("customize"))
    );
}

#[test]
fn rendered_page_keys_keep_root_and_provider_identity() {
    assert_eq!(
        RenderedPage::Root(Tab::Popup).scroll_key(),
        "settings-scroll-customize"
    );
    assert_eq!(
        RenderedPage::Provider(ProviderKind::OpenRouter).page_key(),
        "settings-page-provider-openrouter"
    );
}

#[test]
fn provider_order_follows_popup_order() {
    let order = vec![
        PopupWidgetKind::OpenRouter,
        PopupWidgetKind::TotalSpend,
        PopupWidgetKind::Claude,
        PopupWidgetKind::OpenCodeGo,
    ];
    assert_eq!(
        navigation::provider_order_from_popup(&order),
        vec![
            ProviderKind::OpenRouter,
            ProviderKind::Claude,
            ProviderKind::OpenCodeGo,
        ]
    );
}
