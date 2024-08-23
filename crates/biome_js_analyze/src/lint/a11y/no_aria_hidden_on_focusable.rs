use crate::{services::aria::Aria, JsRuleAction};
use biome_analyze::{
    context::RuleContext, declare_lint_rule, ActionCategory, FixKind, Rule, RuleDiagnostic,
    RuleSource,
};
use biome_console::markup;
use biome_js_syntax::{
    jsx_ext::AnyJsxElement, AnyJsxAttributeValue, JsNumberLiteralExpression,
    JsStringLiteralExpression, JsUnaryExpression,
};
use biome_rowan::{declare_node_union, AstNode, BatchMutationExt};

declare_lint_rule! {
    /// Enforce that aria-hidden="true" is not set on focusable elements.
    ///
    /// `aria-hidden="true"` can be used to hide purely decorative content from screen reader users.
    /// A focusable element with `aria-hidden="true"` can be reached by keyboard.
    /// This can lead to confusion or unexpected behavior for screen reader users.
    ///
    /// ## Example
    ///
    /// ### Invalid
    ///
    /// ```jsx,expect_diagnostic
    /// <div aria-hidden="true" tabIndex="0" />
    /// ```
    ///
    /// ```jsx,expect_diagnostic
    /// <a href="/" aria-hidden="true" />
    /// ```
    ///
    /// ### Valid
    ///
    /// ```jsx
    /// <button aria-hidden="true" tabIndex="-1" />
    /// ```
    ///
    /// ```jsx
    /// <button aria-hidden="true" tabIndex={-1} />
    /// ```
    ///
    /// ```jsx
    /// <div aria-hidden="true"><a href="#"></a></div>
    /// ```
    ///
    /// ## Resources
    ///
    /// - [aria-hidden elements do not contain focusable elements](https://dequeuniversity.com/rules/axe/html/4.4/aria-hidden-focus)
    /// - [Element with aria-hidden has no content in sequential focus navigation](https://www.w3.org/WAI/standards-guidelines/act/rules/6cfa84/proposed/)
    /// - [MDN aria-hidden](https://developer.mozilla.org/en-US/docs/Web/Accessibility/ARIA/Attributes/aria-hidden)
    ///
    pub NoAriaHiddenOnFocusable {
        version: "1.4.0",
        name: "noAriaHiddenOnFocusable",
        language: "jsx",
        sources: &[RuleSource::EslintJsxA11y("no-aria-hidden-on-focusable")],
        recommended: true,
        fix_kind: FixKind::Unsafe,
    }
}

declare_node_union! {
    /// Subset of expressions supported by this rule.
    ///
    /// ## Examples
    ///
    /// - `JsStringLiteralExpression` &mdash; `"5"`
    /// - `JsNumberLiteralExpression` &mdash; `5`
    /// - `JsUnaryExpression` &mdash; `+5` | `-5`
    ///
    pub AnyNumberLikeExpression = JsStringLiteralExpression | JsNumberLiteralExpression | JsUnaryExpression
}

impl AnyNumberLikeExpression {
    /// Returns the value of a number-like expression; it returns the expression
    /// text for literal expressions. However, for unary expressions, it only
    /// returns the value for signed numeric expressions.
    pub(crate) fn value(&self) -> Option<String> {
        match self {
            AnyNumberLikeExpression::JsStringLiteralExpression(string_literal) => {
                return Some(string_literal.inner_string_text().ok()?.to_string());
            }
            AnyNumberLikeExpression::JsNumberLiteralExpression(number_literal) => {
                return Some(number_literal.value_token().ok()?.to_string());
            }
            AnyNumberLikeExpression::JsUnaryExpression(unary_expression) => {
                if unary_expression.is_signed_numeric_literal().ok()? {
                    return Some(unary_expression.text());
                }
            }
        }
        None
    }
}

impl Rule for NoAriaHiddenOnFocusable {
    type Query = Aria<AnyJsxElement>;
    type State = ();
    type Signals = Option<Self::State>;
    type Options = ();

    fn run(ctx: &RuleContext<Self>) -> Self::Signals {
        let node = ctx.query();
        let aria_roles = ctx.aria_roles();
        let element_name = node.name().ok()?.as_jsx_name()?.value_token().ok()?;

        if node.is_element() {
            let aria_hidden_attr = node.find_attribute_by_name("aria-hidden")?;
            let attr_static_val = aria_hidden_attr.as_static_value()?;
            let attr_text = attr_static_val.text();

            let attributes = ctx.extract_attributes(&node.attributes());
            let attributes = ctx.convert_all_attribute_values(attributes);

            if attr_text == "false" {
                return None;
            }

            // if let Some(tabindex_static) =
            //     node.find_attribute_by_name("tabIndex")?.as_static_value()
            // {
            //     let tabindex_text = tabindex_static.text();
            //     let tabindex_val = tabindex_text.trim().parse::<i32>();
            //
            //     if let Ok(num) = tabindex_val {
            //         return (num >= 0).then_some(());
            //     }
            //
            //     if !aria_roles
            //         .is_not_interactive_element(element_name.text_trimmed(), attributes)
            //     {
            //         return Some(());
            //     }
            // }

            // Do stuff if there is a tabIndex attribute
            if let Some(tabindex_attr) = node.find_attribute_by_name("tabIndex") {
                let tabindex_val = tabindex_attr.initializer()?.value().ok()?;

                match tabindex_val {
                    AnyJsxAttributeValue::AnyJsxTag(jsx_tag) => {
                        let value = jsx_tag.text().parse::<i32>();
                        if let Ok(num) = value {
                            return (num >= 0).then_some(());
                        }
                    }
                    AnyJsxAttributeValue::JsxString(jsx_string) => {
                        let value = jsx_string
                            .inner_string_text()
                            .ok()?
                            .to_string()
                            .parse::<i32>();
                        if let Ok(num) = value {
                            return (num >= 0).then_some(());
                        }
                    }
                    AnyJsxAttributeValue::JsxExpressionAttributeValue(value) => {
                        let expression = value.expression().ok()?;
                        let expression_value =
                            AnyNumberLikeExpression::cast(expression.into_syntax())?
                                .value()?
                                .parse::<i32>();
                        if let Ok(num) = expression_value {
                            return (num >= 0).then_some(());
                        }
                    }
                }
            }
        }
        None
    }

    fn diagnostic(ctx: &RuleContext<Self>, _: &Self::State) -> Option<RuleDiagnostic> {
        let node = ctx.query();
        Some(
            RuleDiagnostic::new(
                rule_category!(),
                node.range(),
                markup! {
                    "Disallow "<Emphasis>"aria-hidden=\"true\""</Emphasis>" from being set on focusable elements."
                },
            )
            .note(markup! {
                ""<Emphasis>"aria-hidden"</Emphasis>" should not be set to "<Emphasis>"true"</Emphasis>" on focusable elements because this can lead to confusing behavior for screen reader users."
            }),
        )
    }

    fn action(ctx: &RuleContext<Self>, _: &Self::State) -> Option<JsRuleAction> {
        let node = ctx.query();
        let mut mutation = ctx.root().begin();
        let aria_hidden_attr = node.find_attribute_by_name("aria-hidden")?;
        mutation.remove_node(aria_hidden_attr);
        Some(JsRuleAction::new(
            ActionCategory::QuickFix,
            ctx.metadata().applicability(),
            markup! { "Remove the aria-hidden attribute from the element." }.to_owned(),
            mutation,
        ))
    }
}
