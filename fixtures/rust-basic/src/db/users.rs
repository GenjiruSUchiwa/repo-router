pub struct User {
    pub id: String,
    pub name: String,
}

pub fn find_user(id: &str) -> Option<User> {
    if id == "user_123" {
        Some(User {
            id: id.to_string(),
            name: "Alice".to_string(),
        })
    } else {
        None
    }
}
