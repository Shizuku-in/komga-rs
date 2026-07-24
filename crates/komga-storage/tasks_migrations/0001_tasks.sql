CREATE TABLE IF NOT EXISTS TASK
(
    ID                 varchar  NOT NULL PRIMARY KEY,
    PRIORITY           int      NOT NULL,
    GROUP_ID           varchar  NULL,
    CLASS              varchar  NOT NULL,
    SIMPLE_TYPE        varchar  NOT NULL,
    PAYLOAD            varchar  NOT NULL,
    OWNER              varchar  NULL,
    CREATED_DATE       datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    LAST_MODIFIED_DATE datetime NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx__tasks__owner_group_id ON TASK (OWNER, GROUP_ID);
