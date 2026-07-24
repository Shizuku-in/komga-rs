-- Re-analyze PDF books that were persisted as unsupported before the renderer
-- was available. Existing ready Kotlin analyses already use the compatible
-- page representation and remain untouched.
UPDATE MEDIA
   SET STATUS = 'OUTDATED',
       LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
 WHERE STATUS = 'UNSUPPORTED'
   AND MEDIA_TYPE = 'application/pdf';
