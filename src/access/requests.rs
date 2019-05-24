use diesel;
use diesel::mysql::types::Unsigned;
use diesel::mysql::Mysql;
use diesel::mysql::MysqlConnection;
use diesel::sql_types;
use diesel::ExpressionMethods;
use diesel::QueryDsl;
use diesel::RunQueryDsl;
use diesel::TextExpressionMethods;

use google_signin;

use log::debug;
use log::trace;
use log::warn;

use crate::errors::{Error, ErrorKind};

use crate::search::{NullableSearch, Search};

use super::models::{
    Access, AccessRequest, AccessResponse, JoinedUserAccess,
    JoinedUserAccessList, NewAccess, NewUserAccess, PartialAccess,
    PartialUserAccess, SearchUserAccess, UserAccess, UserAccessRequest,
    UserAccessResponse,
};

use crate::users::models::{NewUser, SearchUser, UserRequest, UserResponse};
use crate::users::requests::handle_user;

use super::schema::access as access_schema;
use super::schema::user_access as user_access_schema;
use crate::users::schema::users as users_schema;

pub fn get_user(
    id_token: &str,
    database_connection: &MysqlConnection,
) -> Option<u64> {
    let mut client = google_signin::Client::new();
    client.audiences.push(String::from(
        "918184954544-jm1aufr31fi6sdjs1140p7p3rouaka14.apps.googleusercontent.com",
    ));

    let id_info = client.verify(id_token);

    match id_info {
        Ok(info) => {
            trace!("Validated token: {:?}", info);
            match info.email {
                Some(email) => {
                    let search_request = UserRequest::SearchUsers(SearchUser {
                        first_name: Search::NoSearch,
                        last_name: Search::NoSearch,
                        banner_id: Search::NoSearch,
                        email: NullableSearch::Exact(email),
                    });
                    match handle_user(
                        search_request,
                        Some(0),
                        database_connection,
                    ) {
                        Ok(found_user) => match found_user {
                            UserResponse::OneUser(user) => Some(user.id),
                            UserResponse::ManyUsers(mut user_list) => {
                                user_list.users.pop().map(|u| u.id)
                            }
                            UserResponse::NoResponse => None,
                        },
                        Err(_e) => None,
                    }
                }
                None => None,
            }
        }
        Err(e) => {
            trace!("Could not validate token: {:?}", e);
            None
        }
    }
}

pub fn check_to_run(
    requesting_user_id: Option<u64>,
    access_type: &str,
    database_connection: &MysqlConnection,
) -> Result<(), Error> {
    trace!(
        "Checking if user {:?} has {}",
        requesting_user_id,
        access_type
    );
    match requesting_user_id {
        Some(user_id) => {
            match check_user_access(
                user_id,
                String::from(access_type),
                database_connection,
            ) {
                Ok(access) => {
                    if access {
                        debug!("Access granted!");
                        Ok(())
                    } else {
                        Err(Error::new(ErrorKind::AccessDenied))
                    }
                }
                Err(e) => Err(e),
            }
        }
        None => Err(Error::new(ErrorKind::AccessDenied)),
    }
}

pub fn handle_access(
    request: AccessRequest,
    requesting_user: Option<u64>,
    database_connection: &MysqlConnection,
) -> Result<AccessResponse, Error> {
    match request {
        AccessRequest::FirstAccess(id_token) => {
            first_access(requesting_user, &id_token, database_connection)
                .map(|_| AccessResponse::NoResponse)
        }
        AccessRequest::GetAccess(id) => {
            match check_to_run(
                requesting_user,
                "GetAccess",
                database_connection,
            ) {
                Ok(()) => get_access(id, database_connection)
                    .map(|a| AccessResponse::OneAccess(a)),
                Err(e) => Err(e),
            }
        }
        AccessRequest::CreateAccess(access) => {
            match check_to_run(
                requesting_user,
                "CreateAccess",
                database_connection,
            ) {
                Ok(()) => create_access(access, database_connection)
                    .map(|a| AccessResponse::OneAccess(a)),
                Err(e) => Err(e),
            }
        }
        AccessRequest::UpdateAccess(id, access) => {
            match check_to_run(
                requesting_user,
                "UpdateAccess",
                database_connection,
            ) {
                Ok(()) => update_access(id, access, database_connection)
                    .map(|_| AccessResponse::NoResponse),
                Err(e) => Err(e),
            }
        }
        AccessRequest::DeleteAccess(id) => {
            match check_to_run(
                requesting_user,
                "DeleteAccess",
                database_connection,
            ) {
                Ok(()) => delete_access(id, database_connection)
                    .map(|_| AccessResponse::NoResponse),
                Err(e) => Err(e),
            }
        }
    }
}

fn first_access(
    requesting_user: Option<u64>,
    id_token: &str,
    database_connection: &MysqlConnection,
) -> Result<(), Error> {
    trace!(
        "First access requested for user: {:?}, id_token: {}",
        requesting_user,
        id_token
    );

    let search = SearchUserAccess {
        access_id: Search::NoSearch,
        user_id: Search::NoSearch,
        permission_level: NullableSearch::NoSearch,
    };

    let non_root_accesses = search_user_access(search, &database_connection)?
        .entries
        .into_iter()
        .filter(|access| access.permission_id != 1)
        .count();

    trace!("Found {} non-root accesses", non_root_accesses);

    if non_root_accesses == 0 {
        let user_id = if let Some(user_id) = requesting_user {
            user_id
        } else {
            let mut client = google_signin::Client::new();
            client.audiences.push(String::from(
                    "918184954544-jm1aufr31fi6sdjs1140p7p3rouaka14.apps.googleusercontent.com",
                    ));

            let info = client.verify(id_token)?;

            trace!("Token verified: {:?}", info);

            let new_user = NewUser {
                first_name: info
                    .given_name
                    .unwrap_or("Not supplied by Google".to_owned()),
                last_name: info
                    .family_name
                    .unwrap_or("Not supplied by Google".to_owned()),
                email: info.email,
                banner_id: 0,
            };

            trace!("New user: {:#?}", new_user);

            let user_response = handle_user(
                UserRequest::CreateUser(new_user),
                Some(0),
                database_connection,
            )?;

            trace!("User response: {:#?}", user_response);

            if let UserResponse::OneUser(user) = user_response {
                user.id
            } else {
                return Err(Error::new(ErrorKind::Database));
            }
        };

        let accesses = access_schema::table
            .filter(access_schema::access_name.ne("RootAccess"))
            .load::<Access>(database_connection)?;

        let new_user_accesses: Vec<_> = accesses
            .into_iter()
            .map(|access| NewUserAccess {
                access_id: access.id,
                user_id: user_id,
                permission_level: None,
            })
            .collect();

        diesel::insert_into(user_access_schema::table)
            .values(new_user_accesses)
            .execute(database_connection)?;

        Ok(())
    } else {
        warn!("First access request attempted, but access has already been setup.");
        Err(Error::new(ErrorKind::AccessDenied))
    }
}

fn get_access(
    id: u64,
    database_connection: &MysqlConnection,
) -> Result<Access, Error> {
    let mut found_access = access_schema::table
        .filter(access_schema::id.eq(id))
        .load::<Access>(database_connection)?;

    match found_access.pop() {
        Some(access) => Ok(access),
        None => Err(Error::new(ErrorKind::NotFound)),
    }
}

fn create_access(
    access: NewAccess,
    database_connection: &MysqlConnection,
) -> Result<Access, Error> {
    diesel::insert_into(access_schema::table)
        .values(access)
        .execute(database_connection)?;

    no_arg_sql_function!(last_insert_id, Unsigned<sql_types::Bigint>);

    let mut inserted_accesses = access_schema::table
        .filter(access_schema::id.eq(last_insert_id))
        //.filter(diesel::dsl::sql("id = LAST_INSERT_ID()"))
        .load::<Access>(database_connection)?;

    if let Some(inserted_access) = inserted_accesses.pop() {
        Ok(inserted_access)
    } else {
        Err(Error::new(ErrorKind::Database))
    }
}

fn update_access(
    id: u64,
    access: PartialAccess,
    database_connection: &MysqlConnection,
) -> Result<(), Error> {
    diesel::update(access_schema::table)
        .filter(access_schema::id.eq(id))
        .set(&access)
        .execute(database_connection)?;
    Ok(())
}

fn delete_access(
    id: u64,
    database_connection: &MysqlConnection,
) -> Result<(), Error> {
    diesel::delete(access_schema::table.filter(access_schema::id.eq(id)))
        .execute(database_connection)?;

    Ok(())
}

pub fn handle_user_access(
    request: UserAccessRequest,
    requesting_user: Option<u64>,
    database_connection: &MysqlConnection,
) -> Result<UserAccessResponse, Error> {
    match request {
        UserAccessRequest::SearchAccess(user_access) => {
            match check_to_run(
                requesting_user,
                "GetUserAccess",
                database_connection,
            ) {
                Ok(()) => search_user_access(user_access, database_connection)
                    .map(|u| UserAccessResponse::ManyUserAccess(u)),
                Err(e) => Err(e),
            }
        }
        UserAccessRequest::GetAccess(permission_id) => {
            match check_to_run(
                requesting_user,
                "GetUserAccess",
                database_connection,
            ) {
                Ok(()) => get_user_access(permission_id, database_connection)
                    .map(|a| UserAccessResponse::OneUserAccess(a)),
                Err(e) => Err(e),
            }
        }
        UserAccessRequest::CheckAccess(user_id, access_name) => {
            check_user_access(user_id, access_name, database_connection)
                .map(|s| UserAccessResponse::AccessState(s))
        }
        UserAccessRequest::CreateAccess(user_access) => {
            match check_to_run(
                requesting_user,
                "CreateUserAccess",
                database_connection,
            ) {
                Ok(()) => create_user_access(user_access, database_connection)
                    .map(|a| UserAccessResponse::OneUserAccess(a)),
                Err(e) => Err(e),
            }
        }
        UserAccessRequest::UpdateAccess(id, user_access) => {
            match check_to_run(
                requesting_user,
                "UpdateUserAccess",
                database_connection,
            ) {
                Ok(()) => {
                    update_user_access(id, user_access, database_connection)
                        .map(|_| UserAccessResponse::NoResponse)
                }
                Err(e) => Err(e),
            }
        }
        UserAccessRequest::DeleteAccess(id) => {
            match check_to_run(
                requesting_user,
                "DeleteUserAccess",
                database_connection,
            ) {
                Ok(()) => delete_user_access(id, database_connection)
                    .map(|_| UserAccessResponse::NoResponse),
                Err(e) => Err(e),
            }
        }
    }
}

fn search_user_access(
    user_access_search: SearchUserAccess,
    database_connection: &MysqlConnection,
) -> Result<JoinedUserAccessList, Error> {
    let mut user_access_query = user_access_schema::table
        .inner_join(access_schema::table)
        .inner_join(users_schema::table)
        .select((
            user_access_schema::permission_id,
            users_schema::id,
            access_schema::id,
            users_schema::first_name,
            users_schema::last_name,
            users_schema::banner_id,
        ))
        .into_boxed::<Mysql>();

    match user_access_search.access_id {
        Search::Partial(s) => {
            user_access_query =
                user_access_query.filter(user_access_schema::access_id.eq(s))
        }

        Search::Exact(s) => {
            user_access_query =
                user_access_query.filter(user_access_schema::access_id.eq(s))
        }

        Search::NoSearch => {}
    }

    match user_access_search.user_id {
        Search::Partial(s) => {
            user_access_query =
                user_access_query.filter(user_access_schema::user_id.eq(s))
        }

        Search::Exact(s) => {
            user_access_query =
                user_access_query.filter(user_access_schema::user_id.eq(s))
        }

        Search::NoSearch => {}
    }

    match user_access_search.permission_level {
        NullableSearch::Partial(s) => {
            user_access_query = user_access_query.filter(
                user_access_schema::permission_level.like(format!("%{}%", s)),
            )
        }

        NullableSearch::Exact(s) => {
            user_access_query = user_access_query
                .filter(user_access_schema::permission_level.eq(s))
        }

        NullableSearch::Some => {
            user_access_query = user_access_query
                .filter(user_access_schema::permission_level.is_not_null());
        }

        NullableSearch::None => {
            user_access_query = user_access_query
                .filter(user_access_schema::permission_level.is_null());
        }

        NullableSearch::NoSearch => {}
    }

    let found_access_entries =
        user_access_query.load::<JoinedUserAccess>(database_connection)?;
    let joined_list = JoinedUserAccessList {
        entries: found_access_entries,
    };

    Ok(joined_list)
}

fn get_user_access(
    permission_id: u64,
    database_connection: &MysqlConnection,
) -> Result<UserAccess, Error> {
    let mut found_user_accesses = user_access_schema::table
        .filter(user_access_schema::permission_id.eq(permission_id))
        .load::<UserAccess>(database_connection)?;

    match found_user_accesses.pop() {
        Some(found_user_access) => Ok(found_user_access),
        None => Err(Error::new(ErrorKind::NotFound)),
    }
}

fn check_user_access(
    user_id: u64,
    access_name: String,
    database_connection: &MysqlConnection,
) -> Result<bool, Error> {
    //checks for root access as a user may have root but not specific permissions
    if access_name != "RootAccess" {
        match check_user_access(
            user_id,
            String::from("RootAccess"),
            database_connection,
        ) {
            Ok(access) => {
                if access {
                    return Ok(true);
                }
            }
            Err(_e) => {}
        }
    }

    //continue with normal permissions check
    let found_user_accesses = user_access_schema::table
        .inner_join(access_schema::table)
        .select((user_access_schema::user_id, access_schema::access_name))
        .filter(user_access_schema::user_id.eq(user_id))
        .filter(access_schema::access_name.eq(access_name))
        .execute(database_connection)?;

    if found_user_accesses != 0 {
        Ok(true)
    } else {
        Ok(false)
    }
}

fn create_user_access(
    user_access: NewUserAccess,
    database_connection: &MysqlConnection,
) -> Result<UserAccess, Error> {
    //find if permission currently exists, should not duplicate (user_id, access_id) pairs
    let found_user_accesses = user_access_schema::table
        .filter(user_access_schema::user_id.eq(user_access.user_id))
        .filter(user_access_schema::access_id.eq(user_access.access_id))
        .execute(database_connection)?;

    if found_user_accesses != 0 {
        return Err(Error::new(ErrorKind::Database));
    }

    //permission most definitely does not exist at this point

    diesel::insert_into(user_access_schema::table)
        .values(user_access)
        .execute(database_connection)?;

    no_arg_sql_function!(last_insert_id, Unsigned<sql_types::Bigint>);

    let mut inserted_accesses = user_access_schema::table
        .filter(user_access_schema::permission_id.eq(last_insert_id))
        //.filter(diesel::dsl::sql("permission_id = LAST_INSERT_ID()"))
        .load::<UserAccess>(database_connection)?;

    if let Some(inserted_access) = inserted_accesses.pop() {
        Ok(inserted_access)
    } else {
        Err(Error::new(ErrorKind::Database))
    }
}

fn update_user_access(
    id: u64,
    user_access: PartialUserAccess,
    database_connection: &MysqlConnection,
) -> Result<(), Error> {
    diesel::update(user_access_schema::table)
        .filter(user_access_schema::permission_id.eq(id))
        .set(&user_access)
        .execute(database_connection)?;

    Ok(())
}

fn delete_user_access(
    id: u64,
    database_connection: &MysqlConnection,
) -> Result<(), Error> {
    diesel::delete(
        user_access_schema::table
            .filter(user_access_schema::permission_id.eq(id)),
    )
    .execute(database_connection)?;

    Ok(())
}
