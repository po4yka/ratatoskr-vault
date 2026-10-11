-- Run as a PostgreSQL administrator after Platform's `01-database-and-roles.sql` has created the database `ratatoskr`, and before the first vault start. Set the password separately with `\password ratatoskr_vault`; no credential belongs in this repository.
-- `create` on the database is all the schema needs: the service creates the `git_vault` schema on its first start and owns everything in it. The role gets nothing in the `public` schema.

do $$
begin
    if not exists (select 1 from pg_roles where rolname = 'ratatoskr_vault') then
        create role ratatoskr_vault login;
    end if;
end
$$;

alter role ratatoskr_vault nosuperuser nocreatedb nocreaterole noreplication nobypassrls;
grant connect, create on database ratatoskr to ratatoskr_vault;

\connect ratatoskr
revoke all on schema public from ratatoskr_vault;
