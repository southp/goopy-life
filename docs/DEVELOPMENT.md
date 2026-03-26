# Development

## Project structure

The folder structure aligns with the service components:

```
poopy-life
    |- backend
        |- public-api
        |- goopy-validator
    |- frontend
```

In the following, we will refer to the ephemeral Ghost instance created on our service as "goopy".

## Backend

The backend is written primarily in Rust, consisting of:

1. Goopy management.
1. The public API.
1. The database schemas and configuration. 
1. Goopy maintanance jobs.
1. Goopy router.

### Goopy management

The core logic of creating and deleting goopy instances. A goopy basically consists of:

1. An identifier; gid for short.
1. A life time, represented as a pair of creation datetime and days to live.
1. A Ghost instance slug.

The actual Ghost instance is maintained by Ghost-CLI. That way our project can likely contribute back to the tool from what we've learned.

Creating a goopy:

1. Generate a `gid` and an unique instance slug.
1. Persist `gid`, creation datetime, the days to live, and the in-progress status flag 
1. Asynchronously create a ghost instance by running `ghost install` under the hood.
1. Update the status flag as "completed" once the instance is successfully created.

Deleting a goopy:
1. Remove the corresponding server configuration and the Ghost instance by running `ghost uninstall` under the hood.
1. Remove the persisted entry identified by `gid`. 

### Database

The database is used for persisting goopies' info as outlined above for the subsequent public API's and the maintanance jobs' usage.

### Public API

The public API is a subcrate that solely focuses on exposing the underlying functionalities as a thin layer of web service.
That means it should only contain the code of an API server; anything related to the ghost instance managing should belong to the main backend crate.

It offers:

1. `POST /goopies` — creating a goopy
1. `GET /goopies/{gid}` — querying the info of a goopy

### Goopy maintenance jobs

* Monitor: A daily job that digests info and send reports through the defined channels.
* Sweeper: A weekly job that removes the expired goopies and the anomolies.

### Goopy router

A server-side module that intercepts accesses to goopies by URLs and routes according to the actual availability and statuses. This is where ephemerality is implemented.

In the production mode, a goopy URL is in the format of {slug}.{production domain}.  In the development mode, it will be just a different port.

* Live: If the current datetime is within the creation datetime + days to live, it's Live.
* Expired: If the current datetime is outside the creation datetime + days to live, it's expired. 
* In-Progress: When the status flag is in-progress.
* Error: When the status flag is any error.
* Non-existent: When the goopy identified by the slug doesn't exist. In production, it'd be filtered out by the reverse proxy server already, but the router itself should still handle this case.

## Frontend

The web interface where users access Goopy.Life built by Next.js.

It offers:

1. Creating a new goopy.
1. Checking the status of a goopy.
1. Getting the URL of a goopy.
1. A description of how our service works.
1. Disclaimer.
1. A link to the GH repo.
1. Copyright claimation.

When a user first creates a goopy, we store the gid in their cookie so they can come back to see their URL later.

## Deployment

All manual at the moment. You are welcome 😎
