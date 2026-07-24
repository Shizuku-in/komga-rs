-- Re-analyze RAR books that were persisted as unsupported before the reader
-- was available. Existing Kotlin databases can contain generic or versioned
-- media types; application/vnd.rar is included for databases populated by
-- detectors that did not normalize the vendor media type.
UPDATE MEDIA
   SET STATUS = 'OUTDATED',
       LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
 WHERE STATUS = 'UNSUPPORTED'
   AND MEDIA_TYPE IN (
       'application/vnd.rar',
       'application/x-rar-compressed',
       'application/x-rar-compressed; version=4',
       'application/x-rar-compressed; version=5'
   );
