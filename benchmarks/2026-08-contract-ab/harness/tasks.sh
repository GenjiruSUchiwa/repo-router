#!/usr/bin/env bash
# 6 repos × 3 tâches — chaque ligne : question|fichier attendu (basename pour le scoring)
# date-fns (TypeScript)
TASKS_DATE_FNS=(
  "Where is the format function defined that converts a date to a string, and what does it do?|format/index.ts"
  "How does parseISO convert a string to a Date? Trace the parsing chain and give the file and line where ISO parsing happens.|parseISO/index.ts"
  "Where is isValid implemented and how does it check that a date is valid?|isValid/index.ts"
)

# Dapper (C#)
TASKS_DAPPER=(
  "Where is Query<T> implemented for a raw IDbConnection, and what does it do?|SqlMapper.cs"
  "Where is Execute implemented for a raw IDbConnection?|SqlMapper.cs"
  "How does Dapper map a column name to a property? Find the naming strategy code.|DefaultTypeMap.cs"
)

# gson (Java)
TASKS_GSON=(
  "Where is fromJson implemented in the Gson class?|Gson.java"
  "Where is GsonBuilder defined and what does its create() method do?|GsonBuilder.java"
  "How does gson serialize an object to JSON? Find the toJson implementation chain.|Gson.java"
)

# serde (Rust)
TASKS_SERDE=(
  "Where is the Serialize trait defined?|ser/mod.rs"
  "Where is the Serializer trait defined?|ser/mod.rs"
  "Where is the derive(Serialize) procedural macro defined?|lib.rs"
)

# axios (JavaScript)
TASKS_AXIOS=(
  "Where is the dispatchRequest function that sends a request through adapters?|dispatchRequest.js"
  "How does axios make an HTTP request in the browser? Find the xhr adapter.|xhr.js"
  "Where is the get method defined on the Axios instance?|Axios.js"
)

# cobra (Go)
TASKS_COBRA=(
  "Where is Execute defined on the Command type?|command.go"
  "Where is AddCommand defined on the Command type?|command.go"
  "Where does cobra parse flags for a command?|command.go"
)
