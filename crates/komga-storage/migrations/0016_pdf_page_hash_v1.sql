-- PDF hashes now use a normalized raw render instead of decoded JPEG output.
-- PAGE_HASH has no algorithm column, so stale rules cannot be identified more
-- narrowly and must be rebuilt with the new PDF hash namespace.
DELETE FROM PAGE_HASH_THUMBNAIL;
DELETE FROM PAGE_HASH;

UPDATE MEDIA_PAGE
   SET FILE_HASH = '', FILE_HASH_ALGORITHM = NULL
 WHERE BOOK_ID IN (
       SELECT BOOK_ID FROM MEDIA WHERE MEDIA_TYPE = 'application/pdf'
 );

CREATE INDEX IF NOT EXISTS idx__media_page__file_hash
    ON MEDIA_PAGE (FILE_HASH);
