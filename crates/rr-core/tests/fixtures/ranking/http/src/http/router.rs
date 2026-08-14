//! Request routing table.

/// One registered route and the handler name it points at.
pub struct Route {
    /// Path pattern the route matches.
    pub pattern: String,
    /// Name of the handler the route dispatches to.
    pub handler: String,
}

/// Adds a route for a path pattern.
pub fn add_route(table: &mut Vec<Route>, pattern: String, handler: String) {
    table.push(Route { pattern, handler });
}

/// Finds the route registered for a request path.
pub fn find_route<'table>(table: &'table [Route], path: &str) -> Option<&'table Route> {
    table.iter().find(|route| route.pattern == path)
}
