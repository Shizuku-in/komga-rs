-- Re-analyze Kindle/Mobipocket files that were persisted as unsupported before
-- AZW and AZW3 filename support was available. Signature validation during
-- analysis still rejects non-BOOKMOBI or DRM-protected content.
UPDATE MEDIA
   SET STATUS = 'OUTDATED',
       LAST_MODIFIED_DATE = CURRENT_TIMESTAMP
 WHERE STATUS = 'UNSUPPORTED'
   AND (
       MEDIA_TYPE IN (
           'application/vnd.amazon.ebook',
           'application/vnd.amazon.mobi8-ebook'
       )
       OR lower((SELECT URL FROM BOOK WHERE BOOK.ID = MEDIA.BOOK_ID)) LIKE '%.azw'
       OR lower((SELECT URL FROM BOOK WHERE BOOK.ID = MEDIA.BOOK_ID)) LIKE '%.azw3'
   );
